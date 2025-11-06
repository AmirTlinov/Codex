use std::collections::BTreeMap;
use std::collections::HashMap;

use anyhow::Result;
use anyhow::anyhow;
use anyhow::bail;
use codex_core::config_types::McpAuthConfig;
use codex_core::config_types::McpHealthcheckConfig;
use codex_core::config_types::McpServerConfig;
use codex_core::config_types::McpServerTransportConfig;
use codex_core::config_types::McpTemplate;
use codex_core::mcp::registry::McpRegistry;
use codex_core::mcp::registry::validate_server_name;
use codex_core::mcp::templates::TemplateCatalog;
use ratatui::style::Stylize;
use ratatui::text::Line;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TransportKind {
    Stdio,
    StreamableHttp,
}

impl TransportKind {
    pub(crate) fn label(self) -> &'static str {
        match self {
            TransportKind::Stdio => "stdio",
            TransportKind::StreamableHttp => "streamable_http",
        }
    }

    pub(crate) fn variants() -> &'static [TransportKind] {
        &[TransportKind::Stdio, TransportKind::StreamableHttp]
    }
}

impl std::fmt::Display for TransportKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.label())
    }
}

#[derive(Debug, Clone, Default)]
pub(crate) struct StdioDraft {
    pub command: String,
    pub args: Vec<String>,
    pub env: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct StreamableHttpDraft {
    pub url: String,
    pub bearer_token_env_var: Option<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct McpWizardDraft {
    pub name: String,
    pub template_id: Option<String>,
    pub display_name: Option<String>,
    pub category: Option<String>,
    pub transport_kind: TransportKind,
    pub stdio: StdioDraft,
    pub http: StreamableHttpDraft,
    pub description: Option<String>,
    pub tags: Vec<String>,
    pub startup_timeout_ms: Option<u64>,
    pub tool_timeout_ms: Option<u64>,
    pub auth: Option<AuthDraft>,
    pub health: Option<HealthDraft>,
    pub metadata: Option<HashMap<String, String>>,
}

impl Default for McpWizardDraft {
    fn default() -> Self {
        Self {
            name: String::new(),
            template_id: None,
            display_name: None,
            category: None,
            transport_kind: TransportKind::Stdio,
            stdio: StdioDraft::default(),
            http: StreamableHttpDraft::default(),
            description: None,
            tags: Vec::new(),
            startup_timeout_ms: None,
            tool_timeout_ms: None,
            auth: None,
            health: None,
            metadata: None,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub(crate) struct AuthDraft {
    pub kind: Option<String>,
    pub secret_ref: Option<String>,
    pub env: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct HealthDraft {
    pub kind: Option<String>,
    pub command: Option<String>,
    pub args: Vec<String>,
    pub timeout_ms: Option<u64>,
    pub interval_seconds: Option<u64>,
    pub endpoint: Option<String>,
    pub protocol: Option<String>,
}

#[allow(dead_code)]
impl McpWizardDraft {
    pub(crate) fn from_existing(name: String, cfg: &McpServerConfig) -> Self {
        let mut draft = Self {
            name,
            ..Self::default()
        };
        draft.populate_from_config(cfg);
        draft
    }

    #[allow(dead_code)]
    pub(crate) fn stdio(&self) -> &StdioDraft {
        &self.stdio
    }

    #[allow(dead_code)]
    pub(crate) fn stdio_mut(&mut self) -> &mut StdioDraft {
        &mut self.stdio
    }

    #[allow(dead_code)]
    pub(crate) fn http(&self) -> &StreamableHttpDraft {
        &self.http
    }

    #[allow(dead_code)]
    pub(crate) fn http_mut(&mut self) -> &mut StreamableHttpDraft {
        &mut self.http
    }

    pub(crate) fn transport_label(&self) -> &'static str {
        self.transport_kind.label()
    }

    pub(crate) fn populate_from_config(&mut self, cfg: &McpServerConfig) {
        self.template_id = cfg.template_id.clone();
        self.display_name = cfg.display_name.clone();
        self.category = cfg.category.clone();
        self.description = cfg.description.clone();
        self.tags = cfg.tags.clone();
        self.metadata = cfg.metadata.clone();
        self.startup_timeout_ms = cfg.startup_timeout_ms();
        self.tool_timeout_ms = cfg.tool_timeout_ms();

        match &cfg.transport {
            McpServerTransportConfig::Stdio {
                command, args, env, ..
            } => {
                self.transport_kind = TransportKind::Stdio;
                self.stdio = StdioDraft {
                    command: command.clone(),
                    args: args.clone(),
                    env: env_to_btree(env),
                };
                self.http = StreamableHttpDraft::default();
            }
            McpServerTransportConfig::StreamableHttp {
                url,
                bearer_token_env_var,
                ..
            } => {
                self.transport_kind = TransportKind::StreamableHttp;
                self.http = StreamableHttpDraft {
                    url: url.clone(),
                    bearer_token_env_var: bearer_token_env_var.clone(),
                };
                self.stdio = StdioDraft::default();
            }
        }

        self.auth = cfg.auth.as_ref().map(AuthDraft::from_config);
        self.health = cfg.healthcheck.as_ref().map(HealthDraft::from_config);
    }

    pub(crate) fn validate(&self) -> Result<()> {
        validate_server_name(&self.name)?;

        match self.transport_kind {
            TransportKind::Stdio => {
                if self.stdio.command.trim().is_empty() {
                    bail!("Command must not be empty");
                }
                for key in self.stdio.env.keys() {
                    if key.trim().is_empty() {
                        bail!("Environment variable keys must not be empty");
                    }
                }
            }
            TransportKind::StreamableHttp => {
                if self.http.url.trim().is_empty() {
                    bail!("URL must not be empty");
                }
                if let Some(var) = &self.http.bearer_token_env_var
                    && var.trim().is_empty()
                {
                    bail!("Bearer token environment variable must not be blank");
                }
            }
        }

        if let Some(auth) = &self.auth {
            if let Some(kind) = &auth.kind
                && kind.trim().is_empty()
            {
                bail!("Authentication type must not be blank");
            }
            for key in auth.env.keys() {
                if key.trim().is_empty() {
                    bail!("Authentication environment keys must not be empty");
                }
            }
        }

        if let Some(health) = &self.health
            && let Some(kind) = &health.kind
            && kind.trim().is_empty()
        {
            bail!("Health check type must not be blank");
        }

        Ok(())
    }

    pub(crate) fn build_server_config(
        &self,
        templates: &TemplateCatalog,
    ) -> Result<McpServerConfig> {
        self.validate()?;

        let mut server = if let Some(template_id) = self.template_id.as_ref() {
            instantiate_template(templates, template_id)?
        } else {
            McpServerConfig::default()
        };

        server.template_id = self.template_id.clone();
        server.display_name = self.display_name.clone();
        server.category = self.category.clone();
        server.description = self.description.clone();
        server.tags = self.tags.clone();
        server.metadata = self.metadata.clone();
        server.set_startup_timeout_ms(self.startup_timeout_ms);
        server.set_tool_timeout_ms(self.tool_timeout_ms);

        match self.transport_kind {
            TransportKind::Stdio => {
                let stdio = self.stdio();
                server.set_stdio_transport(
                    stdio.command.clone(),
                    stdio.args.clone(),
                    map_opt(&stdio.env),
                );
            }
            TransportKind::StreamableHttp => {
                let http = self.http();
                server.set_streamable_http_transport(
                    http.url.clone(),
                    http.bearer_token_env_var.clone(),
                );
            }
        }

        server.auth = self.auth.as_ref().map(AuthDraft::to_config);
        server.healthcheck = self.health.as_ref().map(HealthDraft::to_config);

        Ok(server)
    }

    pub(crate) fn apply_template_config(&mut self, cfg: &McpServerConfig) {
        let name = self.name.clone();
        self.populate_from_config(cfg);
        self.name = name;
    }

    pub(crate) fn summary_lines(&self) -> Vec<Line<'static>> {
        let mut lines: Vec<Line> = Vec::new();
        lines.push("MCP Wizard Summary".bold().into());
        lines.push(Line::from(""));
        lines.push(Line::from(vec!["Name: ".dim(), self.name.clone().into()]));
        if let Some(display_name) = self.display_name.as_ref() {
            lines.push(Line::from(vec![
                "Display name: ".dim(),
                display_name.clone().into(),
            ]));
        }
        if let Some(category) = self.category.as_ref() {
            lines.push(Line::from(vec![
                "Category: ".dim(),
                category.clone().into(),
            ]));
        }
        if let Some(template) = self.template_id.as_ref() {
            lines.push(Line::from(vec![
                "Template: ".dim(),
                template.clone().into(),
            ]));
        } else {
            lines.push(Line::from(vec!["Template: ".dim(), "manual".into()]));
        }
        lines.push(Line::from(vec![
            "Transport: ".dim(),
            self.transport_label().into(),
        ]));

        match self.transport_kind {
            TransportKind::Stdio => {
                lines.push(Line::from(vec![
                    "Command: ".dim(),
                    self.stdio.command.clone().into(),
                ]));
                if !self.stdio.args.is_empty() {
                    lines.push(Line::from(vec![
                        "Args: ".dim(),
                        self.stdio.args.join(" ").into(),
                    ]));
                }
                if !self.stdio.env.is_empty() {
                    lines.push("Env:".dim().into());
                    for (k, v) in &self.stdio.env {
                        lines.push(Line::from(vec!["  • ".into(), format!("{k}={v}").into()]));
                    }
                }
            }
            TransportKind::StreamableHttp => {
                lines.push(Line::from(vec![
                    "URL: ".dim(),
                    self.http.url.clone().into(),
                ]));
                if let Some(var) = self.http.bearer_token_env_var.as_ref() {
                    lines.push(Line::from(vec![
                        "Bearer token env: ".dim(),
                        var.clone().into(),
                    ]));
                }
            }
        }

        if let Some(timeout) = self.startup_timeout_ms {
            lines.push(Line::from(vec![
                "Startup timeout (ms): ".dim(),
                timeout.to_string().into(),
            ]));
        }
        if let Some(timeout) = self.tool_timeout_ms {
            lines.push(Line::from(vec![
                "Tool timeout (ms): ".dim(),
                timeout.to_string().into(),
            ]));
        }
        if let Some(desc) = self.description.as_ref() {
            lines.push(Line::from(vec!["Description: ".dim(), desc.clone().into()]));
        }
        if !self.tags.is_empty() {
            lines.push(Line::from(vec![
                "Tags: ".dim(),
                self.tags.join(", ").into(),
            ]));
        }
        if let Some(auth) = &self.auth {
            lines.extend(auth_lines(auth));
        }
        if let Some(health) = &self.health {
            lines.extend(health_lines(health));
        }
        lines
    }
}

impl AuthDraft {
    fn from_config(cfg: &McpAuthConfig) -> Self {
        Self {
            kind: cfg.kind.clone(),
            secret_ref: cfg.secret_ref.clone(),
            env: env_to_btree(&cfg.env),
        }
    }

    fn to_config(&self) -> McpAuthConfig {
        McpAuthConfig {
            kind: self.kind.clone(),
            secret_ref: self.secret_ref.clone(),
            env: map_opt(&self.env),
        }
    }
}

impl HealthDraft {
    fn from_config(cfg: &McpHealthcheckConfig) -> Self {
        Self {
            kind: cfg.kind.clone(),
            command: cfg.command.clone(),
            args: cfg.args.clone(),
            timeout_ms: cfg.timeout_ms,
            interval_seconds: cfg.interval_seconds,
            endpoint: cfg.endpoint.clone(),
            protocol: cfg.protocol.clone(),
        }
    }

    fn to_config(&self) -> McpHealthcheckConfig {
        McpHealthcheckConfig {
            kind: self.kind.clone(),
            command: self.command.clone(),
            args: self.args.clone(),
            timeout_ms: self.timeout_ms,
            interval_seconds: self.interval_seconds,
            endpoint: self.endpoint.clone(),
            protocol: self.protocol.clone(),
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct TemplateSummary {
    pub id: String,
    pub summary: Option<String>,
    pub category: Option<String>,
}

impl TemplateSummary {
    pub(crate) fn from_template(id: &str, tpl: &McpTemplate) -> Self {
        Self {
            id: id.to_string(),
            summary: tpl.summary.clone(),
            category: tpl.category.clone(),
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct McpServerSnapshot {
    pub name: String,
    pub transport_kind: TransportKind,
    pub stdio: StdioDraft,
    pub http: StreamableHttpDraft,
    pub description: Option<String>,
    pub tags: Vec<String>,
    pub template_id: Option<String>,
    pub display_name: Option<String>,
    pub category: Option<String>,
    pub metadata: Option<HashMap<String, String>>,
    pub auth: Option<AuthDraft>,
    pub health: Option<HealthDraft>,
    pub startup_timeout_ms: Option<u64>,
    pub tool_timeout_ms: Option<u64>,
}

#[allow(dead_code)]
impl McpServerSnapshot {
    pub(crate) fn from_config(name: &str, cfg: &McpServerConfig) -> Self {
        let mut snapshot = Self {
            name: name.to_string(),
            transport_kind: TransportKind::Stdio,
            stdio: StdioDraft::default(),
            http: StreamableHttpDraft::default(),
            description: cfg.description.clone(),
            tags: cfg.tags.clone(),
            template_id: cfg.template_id.clone(),
            display_name: cfg.display_name.clone(),
            category: cfg.category.clone(),
            metadata: cfg.metadata.clone(),
            auth: cfg.auth.as_ref().map(AuthDraft::from_config),
            health: cfg.healthcheck.as_ref().map(HealthDraft::from_config),
            startup_timeout_ms: cfg.startup_timeout_ms(),
            tool_timeout_ms: cfg.tool_timeout_ms(),
        };

        match &cfg.transport {
            McpServerTransportConfig::Stdio {
                command, args, env, ..
            } => {
                snapshot.transport_kind = TransportKind::Stdio;
                snapshot.stdio = StdioDraft {
                    command: command.clone(),
                    args: args.clone(),
                    env: env_to_btree(env),
                };
            }
            McpServerTransportConfig::StreamableHttp {
                url,
                bearer_token_env_var,
                ..
            } => {
                snapshot.transport_kind = TransportKind::StreamableHttp;
                snapshot.http = StreamableHttpDraft {
                    url: url.clone(),
                    bearer_token_env_var: bearer_token_env_var.clone(),
                };
            }
        }

        snapshot
    }

    pub(crate) fn to_config(&self) -> McpServerConfig {
        let mut cfg = McpServerConfig {
            template_id: self.template_id.clone(),
            display_name: self.display_name.clone(),
            category: self.category.clone(),
            description: self.description.clone(),
            tags: self.tags.clone(),
            metadata: self.metadata.clone(),
            ..McpServerConfig::default()
        };
        cfg.set_startup_timeout_ms(self.startup_timeout_ms);
        cfg.set_tool_timeout_ms(self.tool_timeout_ms);

        match self.transport_kind {
            TransportKind::Stdio => {
                cfg.set_stdio_transport(
                    self.stdio.command.clone(),
                    self.stdio.args.clone(),
                    map_opt(&self.stdio.env),
                );
            }
            TransportKind::StreamableHttp => {
                cfg.set_streamable_http_transport(
                    self.http.url.clone(),
                    self.http.bearer_token_env_var.clone(),
                );
            }
        }

        cfg.auth = self.auth.as_ref().map(AuthDraft::to_config);
        cfg.healthcheck = self.health.as_ref().map(HealthDraft::to_config);
        cfg
    }
}

#[derive(Debug, Clone)]
pub(crate) struct McpManagerState {
    pub servers: Vec<McpServerSnapshot>,
    pub template_count: usize,
}

impl McpManagerState {
    pub(crate) fn from_registry(registry: &McpRegistry) -> Self {
        let mut servers: Vec<McpServerSnapshot> = registry
            .servers()
            .map(|(name, cfg)| McpServerSnapshot::from_config(name, cfg))
            .collect();
        servers.sort_by(|a, b| a.name.cmp(&b.name));

        let template_count = registry.templates().len();
        Self {
            servers,
            template_count,
        }
    }
}

pub(crate) fn template_summaries(catalog: &TemplateCatalog) -> Vec<TemplateSummary> {
    let mut summaries: Vec<TemplateSummary> = catalog
        .templates()
        .iter()
        .map(|(id, tpl)| TemplateSummary::from_template(id, tpl))
        .collect();
    summaries.sort_by(|a, b| a.id.cmp(&b.id));
    summaries
}

fn map_opt(source: &BTreeMap<String, String>) -> Option<HashMap<String, String>> {
    if source.is_empty() {
        None
    } else {
        Some(source.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
    }
}

fn env_to_btree(env: &Option<HashMap<String, String>>) -> BTreeMap<String, String> {
    env.as_ref()
        .map(|map| map.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
        .unwrap_or_default()
}

fn instantiate_template(templates: &TemplateCatalog, id: &str) -> Result<McpServerConfig> {
    match templates.instantiate(id) {
        Some(cfg) => Ok(cfg),
        None => Err(anyhow!("Template '{id}' not found")),
    }
}

fn auth_lines(auth: &AuthDraft) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    lines.push("Auth:".dim().into());
    if let Some(kind) = auth.kind.as_ref() {
        lines.push(Line::from(vec!["  • Type: ".into(), kind.clone().into()]));
    }
    if let Some(secret) = auth.secret_ref.as_ref() {
        lines.push(Line::from(vec![
            "  • Secret: ".into(),
            secret.clone().into(),
        ]));
    }
    if !auth.env.is_empty() {
        for (k, v) in &auth.env {
            lines.push(Line::from(vec![
                "     - ".into(),
                format!("{k}={v}").into(),
            ]));
        }
    }
    lines
}

fn health_lines(health: &HealthDraft) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    lines.push("Health:".dim().into());
    if let Some(kind) = health.kind.as_ref() {
        lines.push(Line::from(vec!["  • Type: ".into(), kind.clone().into()]));
    }
    if let Some(cmd) = health.command.as_ref() {
        lines.push(Line::from(vec!["  • Command: ".into(), cmd.clone().into()]));
    }
    if !health.args.is_empty() {
        lines.push(Line::from(vec![
            "  • Args: ".into(),
            health.args.join(" ").into(),
        ]));
    }
    if let Some(endpoint) = health.endpoint.as_ref() {
        lines.push(Line::from(vec![
            "  • Endpoint: ".into(),
            endpoint.clone().into(),
        ]));
    }
    if let Some(timeout) = health.timeout_ms {
        lines.push(Line::from(vec![
            "  • Timeout (ms): ".into(),
            timeout.to_string().into(),
        ]));
    }
    if let Some(interval) = health.interval_seconds {
        lines.push(Line::from(vec![
            "  • Interval (s): ".into(),
            interval.to_string().into(),
        ]));
    }
    lines
}
