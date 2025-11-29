"""Tests for TUI widgets."""

from __future__ import annotations

import pytest

from codex_tui.widgets.approval import is_dangerous_command
from codex_tui.widgets.commands import (
    CommandMatch,
    SlashCommand,
    filter_commands,
    fuzzy_match,
)
from codex_tui.widgets.renderers import truncate_output


class TestDangerousCommands:
    """Tests for dangerous command detection."""

    def test_rm_rf_detected(self) -> None:
        """rm -rf should be detected as dangerous."""
        assert is_dangerous_command("rm -rf /tmp/foo")
        assert is_dangerous_command("rm -fr /var/log")

    def test_sudo_detected(self) -> None:
        """sudo commands should be detected as dangerous."""
        assert is_dangerous_command("sudo apt update")
        assert is_dangerous_command("sudo rm file.txt")

    def test_git_force_push_detected(self) -> None:
        """git push --force should be detected as dangerous."""
        assert is_dangerous_command("git push --force origin main")
        assert is_dangerous_command("git push origin main --force")

    def test_git_reset_hard_detected(self) -> None:
        """git reset --hard should be detected as dangerous."""
        assert is_dangerous_command("git reset --hard HEAD~1")

    def test_safe_commands_not_detected(self) -> None:
        """Safe commands should not be detected as dangerous."""
        assert not is_dangerous_command("ls -la")
        assert not is_dangerous_command("cat file.txt")
        assert not is_dangerous_command("git status")
        assert not is_dangerous_command("python script.py")

    def test_chmod_777_detected(self) -> None:
        """chmod 777 should be detected as dangerous."""
        assert is_dangerous_command("chmod 777 /var/www")

    def test_sql_injection_patterns(self) -> None:
        """SQL injection patterns should be detected."""
        assert is_dangerous_command("drop database users")
        assert is_dangerous_command("TRUNCATE TABLE logs")


class TestFuzzyMatch:
    """Tests for fuzzy matching."""

    def test_exact_match(self) -> None:
        """Exact match should return all indices."""
        result = fuzzy_match("status", "status")
        assert result is not None
        indices, score = result
        assert indices == [0, 1, 2, 3, 4, 5]

    def test_prefix_match(self) -> None:
        """Prefix match should work."""
        result = fuzzy_match("status", "st")
        assert result is not None
        indices, score = result
        assert indices == [0, 1]
        assert score < 0  # Consecutive bonus

    def test_no_match(self) -> None:
        """Non-matching pattern should return None."""
        result = fuzzy_match("status", "xyz")
        assert result is None

    def test_case_insensitive(self) -> None:
        """Matching should be case-insensitive."""
        result = fuzzy_match("Status", "st")
        assert result is not None
        result = fuzzy_match("status", "ST")
        assert result is not None

    def test_empty_pattern(self) -> None:
        """Empty pattern should match everything."""
        result = fuzzy_match("anything", "")
        assert result is not None
        indices, score = result
        assert indices == []
        assert score == 0


class TestFilterCommands:
    """Tests for command filtering."""

    def test_empty_filter_returns_all(self) -> None:
        """Empty filter should return all commands."""
        matches = filter_commands("")
        assert len(matches) == len(list(SlashCommand))

    def test_filter_by_prefix(self) -> None:
        """Filter by prefix should return matching commands."""
        matches = filter_commands("s")
        command_names = [m.command.value for m in matches]
        assert "status" in command_names
        assert "settings" in command_names

    def test_filter_exact(self) -> None:
        """Exact filter should return single match first."""
        matches = filter_commands("quit")
        assert matches[0].command == SlashCommand.QUIT

    def test_filter_no_match(self) -> None:
        """Non-matching filter should return empty."""
        matches = filter_commands("xyzabc")
        assert len(matches) == 0


class TestSlashCommand:
    """Tests for SlashCommand enum."""

    def test_all_commands_have_descriptions(self) -> None:
        """All commands should have descriptions."""
        for cmd in SlashCommand:
            assert cmd.description, f"{cmd.value} has no description"

    def test_quit_exit_same_description(self) -> None:
        """Quit and exit should have the same description."""
        assert SlashCommand.QUIT.description == SlashCommand.EXIT.description

    def test_available_during_task(self) -> None:
        """Some commands should be available during task."""
        assert SlashCommand.STATUS.available_during_task
        assert SlashCommand.HELP.available_during_task
        assert SlashCommand.QUIT.available_during_task

    def test_unavailable_during_task(self) -> None:
        """Some commands should not be available during task."""
        assert not SlashCommand.NEW.available_during_task
        assert not SlashCommand.MODEL.available_during_task


class TestTruncateOutput:
    """Tests for output truncation."""

    def test_short_output_not_truncated(self) -> None:
        """Short output should not be truncated."""
        text = "line1\nline2\nline3"
        result, truncated = truncate_output(text, max_lines=10)
        assert result == text
        assert not truncated

    def test_truncate_by_lines(self) -> None:
        """Long output should be truncated by lines."""
        text = "\n".join(f"line{i}" for i in range(20))
        result, truncated = truncate_output(text, max_lines=5)
        assert len(result.split("\n")) == 5
        assert truncated

    def test_truncate_by_chars(self) -> None:
        """Long output should be truncated by characters."""
        text = "a" * 5000
        result, truncated = truncate_output(text, max_chars=100)
        assert len(result) == 100
        assert truncated


class TestCommandMatch:
    """Tests for CommandMatch dataclass."""

    def test_default_values(self) -> None:
        """Default values should be set correctly."""
        match = CommandMatch(SlashCommand.HELP)
        assert match.command == SlashCommand.HELP
        assert match.indices is None
        assert match.score == 0

    def test_with_indices(self) -> None:
        """CommandMatch should store indices."""
        match = CommandMatch(SlashCommand.STATUS, indices=[0, 1], score=-1)
        assert match.indices == [0, 1]
        assert match.score == -1
