"""Generic selection dialog for settings, models, and other lists.

Provides a reusable modal dialog for selecting from a list of options.
"""

from __future__ import annotations

from dataclasses import dataclass, field
from enum import Enum
from typing import TYPE_CHECKING, Any

from rich.text import Text
from textual.app import ComposeResult
from textual.binding import Binding
from textual.containers import Vertical, VerticalScroll
from textual.screen import ModalScreen
from textual.widgets import Input, Label, Static

if TYPE_CHECKING:
    pass


@dataclass
class SelectionItem:
    """An item in the selection list."""

    id: str
    name: str
    description: str = ""
    shortcut: str | None = None
    is_current: bool = False
    is_enabled: bool = True
    metadata: dict[str, Any] = field(default_factory=dict)


class SelectionItemWidget(Static):
    """Widget representing a single selection item."""

    DEFAULT_CSS = """
    SelectionItemWidget {
        height: auto;
        padding: 0 1;
        margin: 0;
    }

    SelectionItemWidget.selected {
        background: $accent;
    }

    SelectionItemWidget.disabled {
        color: $text-disabled;
    }

    SelectionItemWidget.current {
        background: $success 20%;
    }
    """

    def __init__(self, item: SelectionItem, selected: bool = False) -> None:
        super().__init__()
        self._item = item
        self._selected = selected

        if selected:
            self.add_class("selected")
        if not item.is_enabled:
            self.add_class("disabled")
        if item.is_current:
            self.add_class("current")

    def render(self) -> Text:
        """Render the item."""
        text = Text()

        # Current indicator
        if self._item.is_current:
            text.append("● ", style="green bold")
        else:
            text.append("  ")

        # Shortcut
        if self._item.shortcut:
            text.append(f"[{self._item.shortcut}] ", style="cyan")

        # Name
        style = "bold" if self._selected else ""
        if not self._item.is_enabled:
            style = "dim"
        text.append(self._item.name, style=style)

        # Description
        if self._item.description:
            text.append("  ")
            text.append(self._item.description, style="dim italic")

        return text

    @property
    def item(self) -> SelectionItem:
        """Get the item."""
        return self._item


class SelectionDialog(ModalScreen[SelectionItem | None]):
    """Modal dialog for selecting from a list of options.

    Used for:
    - Model selection
    - Settings configuration
    - Approval mode selection
    - Any other list-based choices
    """

    DEFAULT_CSS = """
    SelectionDialog {
        align: center middle;
    }

    #dialog {
        width: 70%;
        max-width: 80;
        height: auto;
        max-height: 80%;
        border: thick $primary;
        background: $surface;
        padding: 1 2;
    }

    #title {
        text-style: bold;
        color: $primary;
        margin-bottom: 1;
    }

    #subtitle {
        color: $text-muted;
        margin-bottom: 1;
    }

    #search {
        margin-bottom: 1;
    }

    #items-scroll {
        height: auto;
        max-height: 15;
        border: solid $primary-darken-2;
    }

    #footer {
        margin-top: 1;
        color: $text-muted;
        text-align: center;
    }
    """

    BINDINGS = [
        Binding("up", "prev", "Previous", show=False),
        Binding("down", "next", "Next", show=False),
        Binding("k", "prev", "Previous", show=False),
        Binding("j", "next", "Next", show=False),
        Binding("enter", "select", "Select", show=True),
        Binding("escape", "cancel", "Cancel", show=True),
        Binding("1", "shortcut_1", show=False),
        Binding("2", "shortcut_2", show=False),
        Binding("3", "shortcut_3", show=False),
        Binding("4", "shortcut_4", show=False),
        Binding("5", "shortcut_5", show=False),
        Binding("6", "shortcut_6", show=False),
        Binding("7", "shortcut_7", show=False),
        Binding("8", "shortcut_8", show=False),
        Binding("9", "shortcut_9", show=False),
    ]

    def __init__(
        self,
        title: str,
        items: list[SelectionItem],
        subtitle: str | None = None,
        searchable: bool = False,
        search_placeholder: str = "Search...",
    ) -> None:
        super().__init__()
        self._title = title
        self._subtitle = subtitle
        self._items = items
        self._searchable = searchable
        self._search_placeholder = search_placeholder
        self._selected_index = 0
        self._filtered_items: list[SelectionItem] = items.copy()

        # Select current item by default
        for i, item in enumerate(self._filtered_items):
            if item.is_current:
                self._selected_index = i
                break

    def compose(self) -> ComposeResult:
        """Create the dialog layout."""
        with Vertical(id="dialog"):
            yield Label(self._title, id="title")

            if self._subtitle:
                yield Label(self._subtitle, id="subtitle")

            if self._searchable:
                yield Input(placeholder=self._search_placeholder, id="search")

            with VerticalScroll(id="items-scroll"):
                for i, item in enumerate(self._filtered_items):
                    yield SelectionItemWidget(item, selected=(i == self._selected_index))

            yield Static(
                "[dim]↑↓[/] navigate • [dim]Enter[/] select • [dim]Esc[/] cancel",
                id="footer",
            )

    def on_mount(self) -> None:
        """Focus search input if searchable."""
        if self._searchable:
            self.query_one("#search", Input).focus()

    def on_input_changed(self, event: Input.Changed) -> None:
        """Handle search input changes."""
        query = event.value.lower()
        if query:
            self._filtered_items = [
                item for item in self._items
                if query in item.name.lower() or query in item.description.lower()
            ]
        else:
            self._filtered_items = self._items.copy()

        self._selected_index = 0
        self._update_display()

    def _update_display(self) -> None:
        """Update the displayed items."""
        scroll = self.query_one("#items-scroll", VerticalScroll)
        scroll.remove_children()

        for i, item in enumerate(self._filtered_items):
            scroll.mount(SelectionItemWidget(item, selected=(i == self._selected_index)))

    def action_prev(self) -> None:
        """Select previous item."""
        if self._filtered_items:
            self._selected_index = (self._selected_index - 1) % len(self._filtered_items)
            self._update_display()

    def action_next(self) -> None:
        """Select next item."""
        if self._filtered_items:
            self._selected_index = (self._selected_index + 1) % len(self._filtered_items)
            self._update_display()

    def action_select(self) -> None:
        """Confirm selection."""
        if self._filtered_items and 0 <= self._selected_index < len(self._filtered_items):
            item = self._filtered_items[self._selected_index]
            if item.is_enabled:
                self.dismiss(item)

    def action_cancel(self) -> None:
        """Cancel selection."""
        self.dismiss(None)

    def _action_shortcut(self, num: int) -> None:
        """Handle numeric shortcut."""
        shortcut = str(num)
        for item in self._filtered_items:
            if item.shortcut == shortcut and item.is_enabled:
                self.dismiss(item)
                return

    def action_shortcut_1(self) -> None:
        self._action_shortcut(1)

    def action_shortcut_2(self) -> None:
        self._action_shortcut(2)

    def action_shortcut_3(self) -> None:
        self._action_shortcut(3)

    def action_shortcut_4(self) -> None:
        self._action_shortcut(4)

    def action_shortcut_5(self) -> None:
        self._action_shortcut(5)

    def action_shortcut_6(self) -> None:
        self._action_shortcut(6)

    def action_shortcut_7(self) -> None:
        self._action_shortcut(7)

    def action_shortcut_8(self) -> None:
        self._action_shortcut(8)

    def action_shortcut_9(self) -> None:
        self._action_shortcut(9)


# Pre-built dialogs for common use cases


def create_model_selection_dialog(
    current_model: str,
    available_models: list[str] | None = None,
) -> SelectionDialog:
    """Create a model selection dialog."""
    if available_models is None:
        available_models = [
            "gpt-4o",
            "gpt-4o-mini",
            "gpt-4-turbo",
            "gpt-4",
            "o1",
            "o1-mini",
            "o1-preview",
            "o3-mini",
        ]

    items = []
    for i, model in enumerate(available_models, 1):
        items.append(SelectionItem(
            id=model,
            name=model,
            description=_get_model_description(model),
            shortcut=str(i) if i <= 9 else None,
            is_current=(model == current_model),
        ))

    return SelectionDialog(
        title="Select Model",
        subtitle="Choose the AI model to use",
        items=items,
        searchable=True,
        search_placeholder="Filter models...",
    )


def _get_model_description(model: str) -> str:
    """Get description for a model."""
    descriptions = {
        "gpt-4o": "Latest GPT-4 Omni, fast and capable",
        "gpt-4o-mini": "Smaller, faster, cheaper GPT-4",
        "gpt-4-turbo": "GPT-4 Turbo with vision",
        "gpt-4": "Original GPT-4",
        "o1": "Reasoning model, slower but smarter",
        "o1-mini": "Smaller reasoning model",
        "o1-preview": "O1 preview version",
        "o3-mini": "O3 mini reasoning model",
    }
    return descriptions.get(model, "")


class ApprovalMode(Enum):
    """Approval mode settings."""

    SUGGEST = "suggest"
    AUTO_EDIT = "auto-edit"
    FULL_AUTO = "full-auto"


def create_approval_mode_dialog(current_mode: ApprovalMode) -> SelectionDialog:
    """Create an approval mode selection dialog."""
    items = [
        SelectionItem(
            id=ApprovalMode.SUGGEST.value,
            name="Suggest",
            description="Ask for approval before any action",
            shortcut="1",
            is_current=(current_mode == ApprovalMode.SUGGEST),
        ),
        SelectionItem(
            id=ApprovalMode.AUTO_EDIT.value,
            name="Auto Edit",
            description="Auto-approve file edits, ask for commands",
            shortcut="2",
            is_current=(current_mode == ApprovalMode.AUTO_EDIT),
        ),
        SelectionItem(
            id=ApprovalMode.FULL_AUTO.value,
            name="Full Auto",
            description="Auto-approve all safe operations",
            shortcut="3",
            is_current=(current_mode == ApprovalMode.FULL_AUTO),
        ),
    ]

    return SelectionDialog(
        title="Approval Mode",
        subtitle="Choose what Codex can do without asking",
        items=items,
    )


def create_settings_dialog(
    current_model: str,
    current_mode: ApprovalMode,
) -> SelectionDialog:
    """Create a settings hub dialog."""
    items = [
        SelectionItem(
            id="model",
            name="Model",
            description=f"Currently: {current_model}",
            shortcut="1",
        ),
        SelectionItem(
            id="approvals",
            name="Approvals",
            description=f"Currently: {current_mode.value}",
            shortcut="2",
        ),
        SelectionItem(
            id="mcp",
            name="MCP Tools",
            description="View configured MCP servers",
            shortcut="3",
        ),
        SelectionItem(
            id="status",
            name="Status",
            description="View session information",
            shortcut="4",
        ),
        SelectionItem(
            id="diff",
            name="Git Diff",
            description="Show current changes",
            shortcut="5",
        ),
    ]

    return SelectionDialog(
        title="Settings",
        subtitle="Configure Codex options",
        items=items,
    )
