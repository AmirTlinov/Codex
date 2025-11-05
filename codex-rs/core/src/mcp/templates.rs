use std::collections::HashMap;
use std::fs;
use std::path::Path;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::Context;
use anyhow::Result;
use serde_json::Value as JsonValue;

use crate::config_types::McpServerConfig;
use crate::config_types::McpServerTransportConfig;
use crate::config_types::McpTemplate;
use crate::config_types::McpTemplateDefaults;

/// Container for built-in and dynamically loaded MCP templates.
#[derive(Default, Clone)]
pub struct TemplateCatalog {
    templates: HashMap<String, McpTemplate>,
}

impl TemplateCatalog {
    /// Create an empty catalog.
    pub fn empty() -> Self {
        Self {
            templates: HashMap::new(),
        }
    }

    /// Load templates from the default resources directory.
    pub fn load_default() -> Result<Self> {
        let root = Self::default_template_dir();
        if !root.exists() {
            return Ok(Self::empty());
        }

        Self::load_from_dir(&root)
    }

    /// Load templates from a specific directory.
    pub fn load_from_dir(dir: &Path) -> Result<Self> {
        let mut catalog = HashMap::new();
        if !dir.is_dir() {
            return Ok(Self { templates: catalog });
        }

        for entry in
            fs::read_dir(dir).with_context(|| format!("failed to read {}", dir.display()))?
        {
            let entry = entry?;
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) != Some("json") {
                continue;
            }

            let contents = fs::read_to_string(&path)
                .with_context(|| format!("failed to read template {}", path.display()))?;
            match serde_json::from_str::<HashMap<String, McpTemplate>>(&contents) {
                Ok(map) => catalog.extend(map),
                Err(err) => {
                    tracing::warn!("Failed to parse MCP template {}: {err}", path.display());
                }
            }
        }

        Ok(Self { templates: catalog })
    }

    pub fn templates(&self) -> &HashMap<String, McpTemplate> {
        &self.templates
    }

    pub fn instantiate(&self, template_id: &str) -> Option<McpServerConfig> {
        let template = self.templates.get(template_id)?;
        let mut config = McpServerConfig {
            template_id: Some(template_id.to_string()),
            display_name: template.summary.clone(),
            category: template.category.clone(),
            metadata: template.metadata.clone(),
            ..McpServerConfig::default()
        };

        if let Some(defaults) = template.defaults.as_ref() {
            defaults.apply_to(&mut config);
        }

        Some(config)
    }

    fn default_template_dir() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../resources/mcp_templates")
    }
}

/// Utility to validate that a JSON blob can be deserialized as `McpTemplate`.
pub fn validate_template_json(json: &JsonValue) -> Result<McpTemplate> {
    let template: McpTemplate = serde_json::from_value(json.clone())?;
    Ok(template)
}

impl McpTemplateDefaults {
    pub fn apply_to(&self, config: &mut McpServerConfig) {
        if self.command.is_some() || !self.args.is_empty() || self.env.is_some() {
            let (command_slot, args_slot, env_slot) = config.ensure_stdio_mut();

            if let Some(command) = &self.command {
                *command_slot = command.clone();
            }

            if !self.args.is_empty() {
                *args_slot = self.args.clone();
            }

            if let Some(env) = &self.env {
                *env_slot = if env.is_empty() {
                    None
                } else {
                    Some(env.clone())
                };
            }
        }

        if !self.env_vars.is_empty()
            || self.cwd.is_some()
            || self.command.is_some()
            || !self.args.is_empty()
            || self.env.is_some()
        {
            if let McpServerTransportConfig::Stdio { env_vars, cwd, .. } = &mut config.transport {
                if !self.env_vars.is_empty() {
                    *env_vars = self.env_vars.clone();
                }
                if let Some(default_cwd) = &self.cwd {
                    *cwd = Some(default_cwd.clone());
                }
            }
        }

        if self.http_headers.is_some() || self.env_http_headers.is_some() {
            if let McpServerTransportConfig::StreamableHttp {
                http_headers,
                env_http_headers,
                ..
            } = &mut config.transport
            {
                if let Some(headers) = &self.http_headers {
                    *http_headers = if headers.is_empty() {
                        None
                    } else {
                        Some(headers.clone())
                    };
                }
                if let Some(headers) = &self.env_http_headers {
                    *env_http_headers = if headers.is_empty() {
                        None
                    } else {
                        Some(headers.clone())
                    };
                }
            }
        }

        if let Some(auth) = &self.auth {
            config.auth = Some(auth.clone());
        }
        if let Some(health) = &self.healthcheck {
            config.healthcheck = Some(health.clone());
        }
        if !self.tags.is_empty() {
            config.tags = self.tags.clone();
        }
        if let Some(timeout_sec) = self.startup_timeout_sec {
            config.startup_timeout_sec = Duration::try_from_secs_f64(timeout_sec)
                .ok()
                .or(config.startup_timeout_sec);
        } else if let Some(timeout_ms) = self.startup_timeout_ms {
            config.set_startup_timeout_ms(Some(timeout_ms));
        }
        if let Some(tool_timeout_sec) = self.tool_timeout_sec {
            config.tool_timeout_sec = Duration::try_from_secs_f64(tool_timeout_sec)
                .ok()
                .or(config.tool_timeout_sec);
        } else if let Some(tool_timeout_ms) = self.tool_timeout_ms {
            config.set_tool_timeout_ms(Some(tool_timeout_ms));
        }
        if let Some(description) = &self.description {
            config.description = Some(description.clone());
        }
        if let Some(metadata) = &self.metadata {
            config.metadata = if metadata.is_empty() {
                None
            } else {
                Some(metadata.clone())
            };
        }
        if let Some(enabled_tools) = &self.enabled_tools {
            config.enabled_tools = Some(enabled_tools.clone());
        }
        if let Some(disabled_tools) = &self.disabled_tools {
            config.disabled_tools = Some(disabled_tools.clone());
        }
    }
}
