"""Configuration management for Codex.

Loads configuration from ~/.codex/config.toml and ~/.codex/auth.json.
Shares authentication with codex-rs.
"""

from __future__ import annotations

import contextlib
import json
import os
import tomllib
from dataclasses import dataclass, field
from datetime import datetime
from enum import Enum
from pathlib import Path
from typing import Any


class ModelFamily(str, Enum):
    """Model family classification."""

    OPENAI = "openai"
    ANTHROPIC = "anthropic"
    OLLAMA = "ollama"
    CUSTOM = "custom"


class ShellEnvironmentPolicy(str, Enum):
    """Policy for shell environment variables."""

    INHERIT = "inherit"
    MINIMAL = "minimal"


class ReasoningEffort(str, Enum):
    """Reasoning effort for o1/o3 models."""

    LOW = "low"
    MEDIUM = "medium"
    HIGH = "high"


class ReasoningSummary(str, Enum):
    """Reasoning summary mode."""

    NONE = "none"
    AUTO = "auto"
    DETAILED = "detailed"


@dataclass(slots=True)
class AuthTokens:
    """OAuth tokens from auth.json (shared with codex-rs)."""

    id_token: str | None = None
    access_token: str | None = None
    refresh_token: str | None = None
    account_id: str | None = None

    @classmethod
    def from_dict(cls, data: dict[str, Any]) -> AuthTokens:
        return cls(
            id_token=data.get("id_token"),
            access_token=data.get("access_token"),
            refresh_token=data.get("refresh_token"),
            account_id=data.get("account_id"),
        )


@dataclass(slots=True)
class AuthConfig:
    """Authentication configuration from ~/.codex/auth.json."""

    openai_api_key: str | None = None
    tokens: AuthTokens | None = None
    last_refresh: datetime | None = None

    @classmethod
    def load(cls, codex_home: Path) -> AuthConfig | None:
        """Load auth.json from codex home directory."""
        auth_path = codex_home / "auth.json"
        if not auth_path.exists():
            return None

        try:
            with open(auth_path, encoding="utf-8") as f:
                data = json.load(f)

            tokens = None
            if "tokens" in data and data["tokens"]:
                tokens = AuthTokens.from_dict(data["tokens"])

            last_refresh = None
            if "last_refresh" in data and data["last_refresh"]:
                with contextlib.suppress(ValueError, AttributeError):
                    last_refresh = datetime.fromisoformat(
                        data["last_refresh"].replace("Z", "+00:00")
                    )

            return cls(
                openai_api_key=data.get("OPENAI_API_KEY"),
                tokens=tokens,
                last_refresh=last_refresh,
            )
        except (json.JSONDecodeError, OSError):
            return None

    def get_bearer_token(self) -> str | None:
        """Get the bearer token for API requests.

        Uses access_token from OAuth flow (shared with codex-rs).
        """
        if self.tokens and self.tokens.access_token:
            return self.tokens.access_token
        return self.openai_api_key

    def is_chatgpt_auth(self) -> bool:
        """Check if using ChatGPT OAuth (not direct API key)."""
        return bool(self.tokens and self.tokens.access_token)


@dataclass(slots=True)
class McpServerConfig:
    """Configuration for an MCP server."""

    command: str | None = None
    args: list[str] = field(default_factory=list)
    env: dict[str, str] = field(default_factory=dict)
    url: str | None = None
    transport: str = "stdio"
    timeout_secs: int | None = None
    enabled: bool = True

    @classmethod
    def from_dict(cls, data: dict[str, Any]) -> McpServerConfig:
        return cls(
            command=data.get("command"),
            args=data.get("args", []),
            env=data.get("env", {}),
            url=data.get("url"),
            transport=data.get("transport", "stdio"),
            timeout_secs=data.get("timeout_secs"),
            enabled=data.get("enabled", True),
        )


@dataclass(slots=True)
class ModelProviderInfo:
    """Information about a model provider."""

    id: str
    name: str
    base_url: str
    api_key_env_var: str = "OPENAI_API_KEY"
    models: list[str] = field(default_factory=list)

    @classmethod
    def from_dict(cls, id_: str, data: dict[str, Any]) -> ModelProviderInfo:
        return cls(
            id=id_,
            name=data.get("name", id_),
            base_url=data.get("base_url", ""),
            api_key_env_var=data.get("api_key_env_var", "OPENAI_API_KEY"),
            models=data.get("models", []),
        )


@dataclass(slots=True)
class HistoryConfig:
    """Configuration for history persistence."""

    persistence: str = "local"
    sensitive_patterns: list[str] = field(default_factory=list)


@dataclass(slots=True)
class Config:
    """Application configuration loaded from disk and merged with overrides."""

    # Model settings
    model: str = "gpt-4o"
    model_family: ModelFamily = ModelFamily.OPENAI
    model_provider_id: str = "openai"
    model_context_window: int | None = None
    model_max_output_tokens: int | None = None
    model_reasoning_effort: ReasoningEffort | None = None
    model_reasoning_summary: ReasoningSummary = ReasoningSummary.NONE

    # Policies (imported from protocol)
    approval_policy: str = "unless-allow-listed"
    sandbox_policy: str = "workspace-write"

    # Shell settings
    shell_environment_policy: ShellEnvironmentPolicy = ShellEnvironmentPolicy.INHERIT

    # Reasoning display
    hide_agent_reasoning: bool = False
    show_raw_agent_reasoning: bool = False

    # Instructions
    user_instructions: str | None = None
    base_instructions: str | None = None
    developer_instructions: str | None = None

    # Directories
    cwd: Path = field(default_factory=Path.cwd)
    codex_home: Path = field(default_factory=lambda: Path.home() / ".codex")

    # MCP
    mcp_servers: dict[str, McpServerConfig] = field(default_factory=dict)

    # Model providers
    model_providers: dict[str, ModelProviderInfo] = field(default_factory=dict)

    # History
    history: HistoryConfig = field(default_factory=HistoryConfig)

    # Authentication (loaded from auth.json, shared with codex-rs)
    auth: AuthConfig | None = None

    # Feature flags
    tools_web_search_request: bool = False
    include_apply_patch_tool: bool = True

    @classmethod
    def load(cls, overrides: dict[str, Any] | None = None) -> Config:
        """Load configuration from disk and apply overrides."""
        config = cls()

        # Determine codex home
        codex_home_str = os.environ.get("CODEX_HOME")
        if codex_home_str:
            config.codex_home = Path(codex_home_str)

        # Load config file if exists
        config_path = config.codex_home / "config.toml"
        if config_path.exists():
            with open(config_path, "rb") as f:
                data = tomllib.load(f)
            config._apply_toml(data)

        # Load auth.json (shared with codex-rs)
        config.auth = AuthConfig.load(config.codex_home)

        # Apply CLI overrides
        if overrides:
            config._apply_overrides(overrides)

        # Initialize default model providers if not set
        if not config.model_providers:
            config.model_providers = _default_model_providers()

        return config

    def _apply_toml(self, data: dict[str, Any]) -> None:
        """Apply TOML configuration data."""
        if "model" in data:
            self.model = data["model"]
        if "model_provider_id" in data:
            self.model_provider_id = data["model_provider_id"]
        if "approval_policy" in data:
            self.approval_policy = data["approval_policy"]
        if "sandbox_policy" in data:
            self.sandbox_policy = data["sandbox_policy"]
        if "hide_agent_reasoning" in data:
            self.hide_agent_reasoning = data["hide_agent_reasoning"]
        if "show_raw_agent_reasoning" in data:
            self.show_raw_agent_reasoning = data["show_raw_agent_reasoning"]

        # MCP servers
        if "mcp_servers" in data:
            for name, server_data in data["mcp_servers"].items():
                self.mcp_servers[name] = McpServerConfig.from_dict(server_data)

        # Model providers
        if "model_providers" in data:
            for id_, provider_data in data["model_providers"].items():
                self.model_providers[id_] = ModelProviderInfo.from_dict(id_, provider_data)

        # History
        if "history" in data:
            hist = data["history"]
            self.history = HistoryConfig(
                persistence=hist.get("persistence", "local"),
                sensitive_patterns=hist.get("sensitive_patterns", []),
            )

        # Features
        if "features" in data:
            features = data["features"]
            if "web_search_request" in features:
                self.tools_web_search_request = features["web_search_request"]

    def _apply_overrides(self, overrides: dict[str, Any]) -> None:
        """Apply CLI overrides."""
        for key, value in overrides.items():
            if hasattr(self, key):
                setattr(self, key, value)

    def get_api_key(self) -> str | None:
        """Get the API key/token for the current model provider.

        Priority:
        1. Environment variable (OPENAI_API_KEY, ANTHROPIC_API_KEY, etc.)
        2. OAuth access_token from auth.json (shared with codex-rs)
        3. API key from auth.json
        """
        # First check environment variable
        provider = self.model_providers.get(self.model_provider_id)
        if provider:
            env_key = os.environ.get(provider.api_key_env_var)
            if env_key:
                return env_key

        env_key = os.environ.get("OPENAI_API_KEY")
        if env_key:
            return env_key

        # Then check auth.json (shared with codex-rs)
        if self.auth:
            return self.auth.get_bearer_token()

        return None

    def get_base_url(self) -> str:
        """Get the base URL for the current model provider.

        Uses ChatGPT backend API when authenticated via OAuth (shared with codex-rs).
        """
        # Check environment override first
        env_url = os.environ.get("OPENAI_BASE_URL")
        if env_url:
            return env_url

        # ChatGPT OAuth uses special backend API
        if self.auth and self.auth.is_chatgpt_auth():
            return "https://chatgpt.com/backend-api/codex"

        provider = self.model_providers.get(self.model_provider_id)
        if provider:
            return provider.base_url
        return "https://api.openai.com/v1"

    @property
    def providers(self) -> dict[str, ModelProviderInfo]:
        """Alias for model_providers for compatibility."""
        return self.model_providers

    def get_provider(self) -> ModelProviderInfo | None:
        """Get the current model's provider info."""
        # Try to match by model name
        model_lower = self.model.lower()
        for provider in self.model_providers.values():
            for m in provider.models:
                if m.lower() in model_lower or model_lower in m.lower():
                    return provider
        # Fallback to provider_id
        return self.model_providers.get(self.model_provider_id)

    @classmethod
    def from_file(cls, path: Path) -> Config:
        """Load configuration from a specific TOML file."""
        config = cls()
        if path.exists():
            with open(path, "rb") as f:
                data = tomllib.load(f)
            # Handle nested [codex] section
            if "codex" in data:
                config._apply_toml(data["codex"])
            else:
                config._apply_toml(data)
            # Handle providers section at root
            if "providers" in data:
                for id_, provider_data in data["providers"].items():
                    config.model_providers[id_] = ModelProviderInfo.from_dict(id_, provider_data)
        if not config.model_providers:
            config.model_providers = _default_model_providers()
        return config


def _default_model_providers() -> dict[str, ModelProviderInfo]:
    """Return default model provider configurations."""
    return {
        "openai": ModelProviderInfo(
            id="openai",
            name="OpenAI",
            base_url="https://api.openai.com/v1",
            api_key_env_var="OPENAI_API_KEY",
            models=["gpt-4o", "gpt-4o-mini", "gpt-4-turbo", "o1", "o3"],
        ),
        "anthropic": ModelProviderInfo(
            id="anthropic",
            name="Anthropic",
            base_url="https://api.anthropic.com/v1",
            api_key_env_var="ANTHROPIC_API_KEY",
            models=["claude-3-5-sonnet-20241022", "claude-3-opus-20240229"],
        ),
    }


def get_codex_home() -> Path:
    """Get the Codex home directory."""
    env_home = os.environ.get("CODEX_HOME")
    if env_home:
        return Path(env_home)
    return Path.home() / ".codex"


def get_sessions_dir() -> Path:
    """Get the sessions directory."""
    return get_codex_home() / "sessions"
