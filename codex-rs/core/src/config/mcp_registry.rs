use crate::config_types::McpAuthConfig;
use crate::config_types::McpHealthcheckConfig;
use crate::config_types::McpServerConfig;
use crate::config_types::McpServerTransportConfig;
use crate::config_types::McpTemplate;
use crate::config_types::McpTemplateDefaults;
use std::collections::HashMap;
use std::collections::HashSet;
use std::io::Error;
use std::io::ErrorKind;
use std::time::Duration;

pub(crate) const CURRENT_SCHEMA_VERSION: u32 = 2;

#[derive(Debug, Clone)]
pub(crate) struct ResolvedMcpRegistry {
    pub templates: HashMap<String, McpTemplate>,
    pub servers: HashMap<String, McpServerConfig>,
    pub schema_version: u32,
}

pub(crate) fn resolve_registry(
    builtin_templates: &HashMap<String, McpTemplate>,
    user_templates: &HashMap<String, McpTemplate>,
    servers: &HashMap<String, McpServerConfig>,
    stored_schema_version: Option<u32>,
) -> std::io::Result<ResolvedMcpRegistry> {
    let schema_version = normalize_schema_version(stored_schema_version, servers, user_templates)?;

    let mut templates = builtin_templates.clone();
    for (template_id, template) in user_templates {
        templates.insert(template_id.clone(), template.clone());
    }

    validate_template_catalog(&templates)?;

    let mut resolved_servers = HashMap::new();
    for (server_name, server) in servers {
        let resolved = resolve_server(server_name, server, &templates)?;
        resolved_servers.insert(server_name.clone(), resolved);
    }

    Ok(ResolvedMcpRegistry {
        templates,
        servers: resolved_servers,
        schema_version,
    })
}

fn normalize_schema_version(
    stored_schema_version: Option<u32>,
    servers: &HashMap<String, McpServerConfig>,
    user_templates: &HashMap<String, McpTemplate>,
) -> std::io::Result<u32> {
    match stored_schema_version {
        Some(0) => Err(Error::new(
            ErrorKind::InvalidData,
            "mcp_schema_version must be greater than zero",
        )),
        Some(version) if version > CURRENT_SCHEMA_VERSION => Err(Error::new(
            ErrorKind::InvalidData,
            format!(
                "mcp_schema_version {version} is newer than supported version {CURRENT_SCHEMA_VERSION}"
            ),
        )),
        Some(version) => Ok(version),
        None => {
            if servers.is_empty() && user_templates.is_empty() {
                Ok(CURRENT_SCHEMA_VERSION)
            } else {
                Ok(1)
            }
        }
    }
}

fn resolve_server(
    server_name: &str,
    source: &McpServerConfig,
    templates: &HashMap<String, McpTemplate>,
) -> std::io::Result<McpServerConfig> {
    let trimmed_id = source
        .template_id
        .as_ref()
        .map(|raw| raw.trim())
        .map(str::to_string);

    if source.template_id.is_some() && trimmed_id.as_ref().is_none_or(std::string::String::is_empty) {
        return Err(Error::new(
            ErrorKind::InvalidData,
            format!("MCP server `{server_name}` has an empty template_id"),
        ));
    }

    let mut merged = source.clone();
    merged.template_id = trimmed_id.clone();

    let Some(template_id) = trimmed_id else {
        return Ok(merged);
    };

    let template = templates.get(&template_id).ok_or_else(|| {
        Error::new(
            ErrorKind::InvalidData,
            format!("MCP server `{server_name}` references unknown template `{template_id}`"),
        )
    })?;

    apply_template_defaults(server_name, &template_id, &mut merged, template)?;

    Ok(merged)
}

fn apply_template_defaults(
    server_name: &str,
    template_id: &str,
    merged: &mut McpServerConfig,
    template: &McpTemplate,
) -> std::io::Result<()> {
    if let Some(defaults) = &template.defaults {
        let transport_snapshot = merged.transport.clone();
        let transport_is_stdio =
            matches!(transport_snapshot, McpServerTransportConfig::Stdio { .. });
        let transport_is_http = matches!(
            transport_snapshot,
            McpServerTransportConfig::StreamableHttp { .. }
        );

        if defaults_contains_http_fields(defaults) && transport_is_stdio {
            return Err(Error::new(
                ErrorKind::InvalidData,
                format!(
                    "Template `{template_id}` defines HTTP defaults but server `{server_name}` uses stdio transport"
                ),
            ));
        }

        if defaults_contains_stdio_fields(defaults) && transport_is_http {
            return Err(Error::new(
                ErrorKind::InvalidData,
                format!(
                    "Template `{template_id}` defines stdio defaults but server `{server_name}` uses streamable_http transport"
                ),
            ));
        }

        match (&mut merged.transport, transport_snapshot) {
            (
                McpServerTransportConfig::Stdio {
                    command,
                    args,
                    env,
                    env_vars,
                    cwd,
                },
                McpServerTransportConfig::Stdio {
                    env: source_env,
                    env_vars: source_env_vars,
                    ..
                },
            ) => {
                if command.trim().is_empty() {
                    if let Some(default_command) = defaults.command.as_deref() {
                        let Some(normalized) = trimmed_non_empty(default_command) else {
                            return Err(Error::new(
                                ErrorKind::InvalidData,
                                format!(
                                    "Template `{template_id}` stdio command is empty for server `{server_name}`"
                                ),
                            ));
                        };
                        *command = normalized;
                    }
                } else {
                    *command = command.trim().to_string();
                }

                if args.is_empty() && !defaults.args.is_empty() {
                    *args = normalized_list(&defaults.args);
                } else if !args.is_empty() {
                    *args = normalized_list(args);
                }

                *env = merge_env_map(defaults.env.as_ref(), source_env.as_ref());
                *env_vars = merge_env_vars(&defaults.env_vars, &source_env_vars);

                if cwd.is_none()
                    && let Some(default_cwd) = defaults.cwd.as_ref() {
                        *cwd = Some(default_cwd.clone());
                    }
            }
            (
                McpServerTransportConfig::StreamableHttp {
                    http_headers,
                    env_http_headers,
                    ..
                },
                McpServerTransportConfig::StreamableHttp {
                    http_headers: source_http_headers,
                    env_http_headers: source_env_headers,
                    ..
                },
            ) => {
                *http_headers =
                    merge_env_map(defaults.http_headers.as_ref(), source_http_headers.as_ref());
                *env_http_headers = merge_env_map(
                    defaults.env_http_headers.as_ref(),
                    source_env_headers.as_ref(),
                );
            }
            _ => {}
        }

        merged.auth = merge_auth(defaults.auth.as_ref(), merged.auth.as_ref());
        merged.healthcheck =
            merge_healthcheck(defaults.healthcheck.as_ref(), merged.healthcheck.as_ref());
        merged.tags = merge_tags(&defaults.tags, &merged.tags);
        merged.metadata = merge_metadata(defaults.metadata.as_ref(), merged.metadata.as_ref());
        merged.description =
            pick_string(merged.description.as_ref(), defaults.description.as_ref());
        merged.startup_timeout_sec = merge_duration(
            merged.startup_timeout_sec,
            defaults.startup_timeout_sec,
            "defaults.startup_timeout_sec",
            template_id,
        )?;
        merged.tool_timeout_sec = merge_duration(
            merged.tool_timeout_sec,
            defaults.tool_timeout_sec,
            "defaults.tool_timeout_sec",
            template_id,
        )?;
        merged.enabled_tools = merge_optional_vec(
            merged.enabled_tools.as_ref(),
            defaults.enabled_tools.as_ref(),
        );
        merged.disabled_tools = merge_optional_vec(
            merged.disabled_tools.as_ref(),
            defaults.disabled_tools.as_ref(),
        );
    } else if let McpServerTransportConfig::Stdio { command, args, .. } = &mut merged.transport {
        if command.trim().is_empty() {
            return Err(Error::new(
                ErrorKind::InvalidData,
                format!(
                    "MCP server `{server_name}` uses template `{template_id}` but no command is defined"
                ),
            ));
        }
        *command = command.trim().to_string();
        if !args.is_empty() {
            *args = normalized_list(args);
        }
    }

    match &mut merged.transport {
        McpServerTransportConfig::Stdio { command, .. } => {
            if command.trim().is_empty() {
                return Err(Error::new(
                    ErrorKind::InvalidData,
                    format!(
                        "MCP server `{server_name}` resolved command is empty after applying template `{template_id}`"
                    ),
                ));
            }
            *command = command.trim().to_string();
        }
        McpServerTransportConfig::StreamableHttp { .. } => {}
    }

    Ok(())
}

fn validate_template_catalog(templates: &HashMap<String, McpTemplate>) -> std::io::Result<()> {
    for (template_id, template) in templates {
        if template_id.trim().is_empty() {
            return Err(Error::new(
                ErrorKind::InvalidData,
                "Template identifiers must not be empty",
            ));
        }
        if template_id.trim() != template_id {
            return Err(Error::new(
                ErrorKind::InvalidData,
                format!(
                    "Template identifier `{template_id}` must not contain leading or trailing whitespace"
                ),
            ));
        }
        validate_template(template_id, template)?;
    }
    Ok(())
}

fn validate_template(template_id: &str, template: &McpTemplate) -> std::io::Result<()> {
    if let Some(version) = template.version.as_deref() {
        ensure_trimmed_non_empty(version, "version", template_id)?;
    }
    if let Some(summary) = template.summary.as_deref() {
        ensure_trimmed_non_empty(summary, "summary", template_id)?;
    }
    if let Some(category) = template.category.as_deref() {
        ensure_trimmed_non_empty(category, "category", template_id)?;
    }
    if let Some(metadata) = template.metadata.as_ref() {
        ensure_map_entries_non_empty(metadata, "metadata", template_id)?;
    }
    if let Some(defaults) = template.defaults.as_ref() {
        validate_template_defaults(template_id, defaults)?;
    }
    Ok(())
}

fn validate_template_defaults(
    template_id: &str,
    defaults: &McpTemplateDefaults,
) -> std::io::Result<()> {
    if let Some(command) = defaults.command.as_deref() {
        ensure_trimmed_non_empty(command, "defaults.command", template_id)?;
    }
    ensure_list_entries_non_empty(&defaults.args, "defaults.args", template_id)?;
    if let Some(env) = defaults.env.as_ref() {
        ensure_map_entries_non_empty(env, "defaults.env", template_id)?;
    }
    ensure_list_entries_non_empty(&defaults.env_vars, "defaults.env_vars", template_id)?;
    if let Some(auth) = defaults.auth.as_ref() {
        validate_auth(template_id, auth)?;
    }
    if let Some(health) = defaults.healthcheck.as_ref() {
        validate_healthcheck(template_id, health)?;
    }
    ensure_list_entries_non_empty(&defaults.tags, "defaults.tags", template_id)?;
    if let Some(value) = defaults.startup_timeout_sec {
        ensure_positive_timeout(value, "defaults.startup_timeout_sec", template_id)?;
    }
    if let Some(value) = defaults.tool_timeout_sec {
        ensure_positive_timeout(value, "defaults.tool_timeout_sec", template_id)?;
    }
    if let Some(values) = defaults.enabled_tools.as_ref() {
        ensure_list_entries_non_empty(values, "defaults.enabled_tools", template_id)?;
    }
    if let Some(values) = defaults.disabled_tools.as_ref() {
        ensure_list_entries_non_empty(values, "defaults.disabled_tools", template_id)?;
    }
    if let Some(description) = defaults.description.as_deref() {
        ensure_trimmed_non_empty(description, "defaults.description", template_id)?;
    }
    if let Some(metadata) = defaults.metadata.as_ref() {
        ensure_map_entries_non_empty(metadata, "defaults.metadata", template_id)?;
    }
    if let Some(headers) = defaults.http_headers.as_ref() {
        ensure_map_entries_non_empty(headers, "defaults.http_headers", template_id)?;
    }
    if let Some(headers) = defaults.env_http_headers.as_ref() {
        ensure_map_entries_non_empty(headers, "defaults.env_http_headers", template_id)?;
    }
    Ok(())
}

fn validate_auth(template_id: &str, auth: &McpAuthConfig) -> std::io::Result<()> {
    if let Some(kind) = auth.kind.as_deref() {
        ensure_trimmed_non_empty(kind, "defaults.auth.type", template_id)?;
    }
    if let Some(secret_ref) = auth.secret_ref.as_deref() {
        ensure_trimmed_non_empty(secret_ref, "defaults.auth.secret_ref", template_id)?;
    }
    if let Some(env) = auth.env.as_ref() {
        ensure_map_entries_non_empty(env, "defaults.auth.env", template_id)?;
    }
    Ok(())
}

fn validate_healthcheck(template_id: &str, health: &McpHealthcheckConfig) -> std::io::Result<()> {
    if let Some(kind) = health.kind.as_deref() {
        ensure_trimmed_non_empty(kind, "defaults.healthcheck.type", template_id)?;
    }
    if let Some(command) = health.command.as_deref() {
        ensure_trimmed_non_empty(command, "defaults.healthcheck.command", template_id)?;
    }
    ensure_list_entries_non_empty(&health.args, "defaults.healthcheck.args", template_id)?;
    if let Some(endpoint) = health.endpoint.as_deref() {
        ensure_trimmed_non_empty(endpoint, "defaults.healthcheck.endpoint", template_id)?;
    }
    if let Some(protocol) = health.protocol.as_deref() {
        ensure_trimmed_non_empty(protocol, "defaults.healthcheck.protocol", template_id)?;
    }
    Ok(())
}

fn ensure_trimmed_non_empty(value: &str, field: &str, template_id: &str) -> std::io::Result<()> {
    if value.trim().is_empty() {
        return Err(Error::new(
            ErrorKind::InvalidData,
            format!("Template `{template_id}` {field} must not be empty"),
        ));
    }
    Ok(())
}

fn ensure_map_entries_non_empty(
    map: &HashMap<String, String>,
    field: &str,
    template_id: &str,
) -> std::io::Result<()> {
    for (key, value) in map {
        if key.trim().is_empty() {
            return Err(Error::new(
                ErrorKind::InvalidData,
                format!("Template `{template_id}` {field} contains an empty key"),
            ));
        }
        if value.trim().is_empty() {
            return Err(Error::new(
                ErrorKind::InvalidData,
                format!("Template `{template_id}` {field} contains an empty value for key `{key}`"),
            ));
        }
    }
    Ok(())
}

fn ensure_list_entries_non_empty(
    values: &[String],
    field: &str,
    template_id: &str,
) -> std::io::Result<()> {
    for value in values {
        if value.trim().is_empty() {
            return Err(Error::new(
                ErrorKind::InvalidData,
                format!("Template `{template_id}` {field} contains an empty entry"),
            ));
        }
    }
    Ok(())
}

fn ensure_positive_timeout(value: f64, field: &str, template_id: &str) -> std::io::Result<()> {
    if value <= 0.0 {
        return Err(Error::new(
            ErrorKind::InvalidData,
            format!("Template `{template_id}` {field} must be positive (got {value})"),
        ));
    }
    Ok(())
}

fn defaults_contains_stdio_fields(defaults: &McpTemplateDefaults) -> bool {
    defaults.command.is_some()
        || !defaults.args.is_empty()
        || defaults.env.is_some()
        || !defaults.env_vars.is_empty()
        || defaults.cwd.is_some()
}

fn defaults_contains_http_fields(defaults: &McpTemplateDefaults) -> bool {
    defaults.http_headers.is_some() || defaults.env_http_headers.is_some()
}

fn merge_auth(
    template: Option<&McpAuthConfig>,
    server: Option<&McpAuthConfig>,
) -> Option<McpAuthConfig> {
    match (template, server) {
        (None, None) => None,
        (None, Some(value)) => Some(normalize_auth(value)),
        (Some(value), None) => Some(normalize_auth(value)),
        (Some(template_auth), Some(server_auth)) => {
            let merged = McpAuthConfig {
                kind: pick_string(server_auth.kind.as_ref(), template_auth.kind.as_ref()),
                secret_ref: pick_string(
                    server_auth.secret_ref.as_ref(),
                    template_auth.secret_ref.as_ref(),
                ),
                env: merge_env_map(template_auth.env.as_ref(), server_auth.env.as_ref()),
            };

            if merged.kind.is_none() && merged.secret_ref.is_none() && merged.env.is_none() {
                None
            } else {
                Some(merged)
            }
        }
    }
}

fn normalize_auth(auth: &McpAuthConfig) -> McpAuthConfig {
    McpAuthConfig {
        kind: trimmed_or_none(auth.kind.as_ref()),
        secret_ref: trimmed_or_none(auth.secret_ref.as_ref()),
        env: merge_env_map(None, auth.env.as_ref()),
    }
}

fn merge_healthcheck(
    template: Option<&McpHealthcheckConfig>,
    server: Option<&McpHealthcheckConfig>,
) -> Option<McpHealthcheckConfig> {
    match (template, server) {
        (None, None) => None,
        (None, Some(value)) => Some(normalize_healthcheck(value)),
        (Some(value), None) => Some(normalize_healthcheck(value)),
        (Some(template_health), Some(server_health)) => {
            let merged = McpHealthcheckConfig {
                kind: pick_string(server_health.kind.as_ref(), template_health.kind.as_ref()),
                command: pick_string(
                    server_health.command.as_ref(),
                    template_health.command.as_ref(),
                ),
                args: if server_health.args.is_empty() {
                    normalized_list(&template_health.args)
                } else {
                    normalized_list(&server_health.args)
                },
                timeout_ms: server_health.timeout_ms.or(template_health.timeout_ms),
                interval_seconds: server_health
                    .interval_seconds
                    .or(template_health.interval_seconds),
                endpoint: pick_string(
                    server_health.endpoint.as_ref(),
                    template_health.endpoint.as_ref(),
                ),
                protocol: pick_string(
                    server_health.protocol.as_ref(),
                    template_health.protocol.as_ref(),
                ),
            };
            if merged.kind.is_none()
                && merged.command.is_none()
                && merged.args.is_empty()
                && merged.timeout_ms.is_none()
                && merged.interval_seconds.is_none()
                && merged.endpoint.is_none()
                && merged.protocol.is_none()
            {
                None
            } else {
                Some(merged)
            }
        }
    }
}

fn normalize_healthcheck(health: &McpHealthcheckConfig) -> McpHealthcheckConfig {
    McpHealthcheckConfig {
        kind: trimmed_or_none(health.kind.as_ref()),
        command: trimmed_or_none(health.command.as_ref()),
        args: normalized_list(&health.args),
        timeout_ms: health.timeout_ms,
        interval_seconds: health.interval_seconds,
        endpoint: trimmed_or_none(health.endpoint.as_ref()),
        protocol: trimmed_or_none(health.protocol.as_ref()),
    }
}

fn merge_tags(template_tags: &[String], server_tags: &[String]) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut merged = Vec::new();
    for tag in template_tags {
        if let Some(value) = trimmed_non_empty(tag)
            && seen.insert(value.clone()) {
                merged.push(value);
            }
    }
    for tag in server_tags {
        if let Some(value) = trimmed_non_empty(tag)
            && seen.insert(value.clone()) {
                merged.push(value);
            }
    }
    merged
}

fn merge_metadata(
    template_metadata: Option<&HashMap<String, String>>,
    server_metadata: Option<&HashMap<String, String>>,
) -> Option<HashMap<String, String>> {
    merge_env_map(template_metadata, server_metadata)
}

fn merge_env_map(
    template: Option<&HashMap<String, String>>,
    server: Option<&HashMap<String, String>>,
) -> Option<HashMap<String, String>> {
    let mut combined = HashMap::new();
    if let Some(entries) = template {
        for (key, value) in entries {
            if let (Some(normalized_key), Some(normalized_value)) =
                (trimmed_non_empty(key), trimmed_non_empty(value))
            {
                combined.insert(normalized_key, normalized_value);
            }
        }
    }
    if let Some(entries) = server {
        for (key, value) in entries {
            if let (Some(normalized_key), Some(normalized_value)) =
                (trimmed_non_empty(key), trimmed_non_empty(value))
            {
                combined.insert(normalized_key, normalized_value);
            }
        }
    }
    if combined.is_empty() {
        None
    } else {
        Some(combined)
    }
}

fn merge_env_vars(template_vars: &[String], server_vars: &[String]) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut merged = Vec::new();
    for value in template_vars {
        if let Some(normalized) = trimmed_non_empty(value)
            && seen.insert(normalized.clone()) {
                merged.push(normalized);
            }
    }
    for value in server_vars {
        if let Some(normalized) = trimmed_non_empty(value)
            && seen.insert(normalized.clone()) {
                merged.push(normalized);
            }
    }
    merged
}

fn merge_duration(
    existing: Option<Duration>,
    default_secs: Option<f64>,
    field: &str,
    template_id: &str,
) -> std::io::Result<Option<Duration>> {
    if existing.is_some() {
        return Ok(existing);
    }
    let Some(value) = default_secs else {
        return Ok(None);
    };
    ensure_positive_timeout(value, field, template_id)?;
    let duration = Duration::try_from_secs_f64(value).map_err(|err| {
        Error::new(
            ErrorKind::InvalidData,
            format!("Template `{template_id}` {field} is invalid: {err}"),
        )
    })?;
    Ok(Some(duration))
}

fn merge_optional_vec(
    server: Option<&Vec<String>>,
    template: Option<&Vec<String>>,
) -> Option<Vec<String>> {
    match (server, template) {
        (Some(values), _) => Some(normalized_list(values)),
        (None, Some(values)) => Some(normalized_list(values)),
        (None, None) => None,
    }
}

fn pick_string(primary: Option<&String>, fallback: Option<&String>) -> Option<String> {
    trimmed_or_none(primary).or_else(|| trimmed_or_none(fallback))
}

fn trimmed_or_none(value: Option<&String>) -> Option<String> {
    value.and_then(trimmed_non_empty)
}

fn trimmed_non_empty(value: impl AsRef<str>) -> Option<String> {
    let raw = value.as_ref();
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

fn normalized_list(values: &[String]) -> Vec<String> {
    values
        .iter()
        .filter_map(trimmed_non_empty)
        .collect()
}
