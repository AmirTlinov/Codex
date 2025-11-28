"""Tests for codex_patch module."""

import pytest
from pathlib import Path

from codex_patch import (
    parse_patch,
    ParseError,
    Hunk,
    HunkType,
    PatchApplier,
    ApplyStatus,
    SymbolResolver,
    SymbolPath,
    SymbolNotFoundError,
)


class TestParsePatch:
    """Tests for patch parsing."""

    def test_parse_add_file(self) -> None:
        """Test parsing Add File operation."""
        patch = """*** Begin Patch
*** Add File: src/hello.py
+def hello():
+    print("Hello!")
*** End Patch
"""
        hunks = parse_patch(patch)

        assert len(hunks) == 1
        assert hunks[0].type == HunkType.ADD_FILE
        assert hunks[0].path == Path("src/hello.py")
        assert hunks[0].contents == 'def hello():\n    print("Hello!")'

    def test_parse_delete_file(self) -> None:
        """Test parsing Delete File operation."""
        patch = """*** Begin Patch
*** Delete File: old_file.py
*** End Patch
"""
        hunks = parse_patch(patch)

        assert len(hunks) == 1
        assert hunks[0].type == HunkType.DELETE_FILE
        assert hunks[0].path == Path("old_file.py")

    def test_parse_update_file(self) -> None:
        """Test parsing Update File operation with hunks."""
        patch = """*** Begin Patch
*** Update File: main.py
@@ def greet():
-    print("hi")
+    print("Hello!")
*** End Patch
"""
        hunks = parse_patch(patch)

        assert len(hunks) == 1
        assert hunks[0].type == HunkType.UPDATE_FILE
        assert hunks[0].path == Path("main.py")
        assert len(hunks[0].chunks) == 1
        assert hunks[0].chunks[0].context == "def greet():"
        assert hunks[0].chunks[0].old_lines == ['    print("hi")']
        assert hunks[0].chunks[0].new_lines == ['    print("Hello!")']

    def test_parse_update_file_with_move(self) -> None:
        """Test Update File with rename."""
        patch = """*** Begin Patch
*** Update File: old_name.py
*** Move to: new_name.py
@@
-old content
+new content
*** End Patch
"""
        hunks = parse_patch(patch)

        assert len(hunks) == 1
        assert hunks[0].move_to == Path("new_name.py")

    def test_parse_symbol_insert_before(self) -> None:
        """Test parsing Insert Before Symbol."""
        patch = """*** Begin Patch
*** Insert Before Symbol: utils.py::MyClass::method
+# This is a comment
+@decorator
*** End Patch
"""
        hunks = parse_patch(patch)

        assert len(hunks) == 1
        assert hunks[0].type == HunkType.INSERT_BEFORE_SYMBOL
        assert hunks[0].path == Path("utils.py")
        assert hunks[0].symbol_path == "MyClass::method"
        assert hunks[0].new_lines == ["# This is a comment", "@decorator"]

    def test_parse_symbol_insert_after(self) -> None:
        """Test parsing Insert After Symbol."""
        patch = """*** Begin Patch
*** Insert After Symbol: lib.py::process
+
+def helper():
+    pass
*** End Patch
"""
        hunks = parse_patch(patch)

        assert len(hunks) == 1
        assert hunks[0].type == HunkType.INSERT_AFTER_SYMBOL
        assert hunks[0].symbol_path == "process"

    def test_parse_replace_symbol_body(self) -> None:
        """Test parsing Replace Symbol Body."""
        patch = """*** Begin Patch
*** Replace Symbol Body: handlers.py::Handler::process
+    self.validate()
+    return self.execute()
*** End Patch
"""
        hunks = parse_patch(patch)

        assert len(hunks) == 1
        assert hunks[0].type == HunkType.REPLACE_SYMBOL_BODY
        assert hunks[0].symbol_path == "Handler::process"

    def test_parse_multiple_operations(self) -> None:
        """Test parsing multiple operations in one patch."""
        patch = """*** Begin Patch
*** Add File: new.py
+# New file
*** Delete File: old.py
*** Update File: existing.py
@@
-old
+new
*** End Patch
"""
        hunks = parse_patch(patch)

        assert len(hunks) == 3
        assert hunks[0].type == HunkType.ADD_FILE
        assert hunks[1].type == HunkType.DELETE_FILE
        assert hunks[2].type == HunkType.UPDATE_FILE

    def test_parse_error_missing_begin(self) -> None:
        """Test error on missing Begin Patch."""
        with pytest.raises(ParseError) as exc_info:
            parse_patch("*** Add File: foo.py\n+content\n*** End Patch")
        assert "Begin Patch" in str(exc_info.value)

    def test_parse_empty_patch(self) -> None:
        """Test parsing empty patch."""
        patch = """*** Begin Patch
*** End Patch
"""
        hunks = parse_patch(patch)
        assert len(hunks) == 0


class TestSymbolResolver:
    """Tests for Python symbol resolution."""

    def test_resolve_function(self) -> None:
        """Test resolving a function."""
        source = '''
def greet(name: str) -> str:
    """Say hello."""
    return f"Hello, {name}!"
'''
        resolver = SymbolResolver(source)
        symbol = resolver.resolve("greet")

        assert symbol.name == "greet"
        assert symbol.location.start_line == 2
        assert symbol.location.end_line == 4

    def test_resolve_class(self) -> None:
        """Test resolving a class."""
        source = '''
class MyClass:
    """A class."""

    def __init__(self):
        pass

    def method(self):
        return 42
'''
        resolver = SymbolResolver(source)
        symbol = resolver.resolve("MyClass")

        assert symbol.name == "MyClass"
        assert symbol.location.start_line == 2

    def test_resolve_nested_method(self) -> None:
        """Test resolving a method inside a class."""
        source = '''
class Calculator:
    def add(self, a, b):
        return a + b

    def multiply(self, a, b):
        return a * b
'''
        resolver = SymbolResolver(source)
        symbol = resolver.resolve("Calculator::multiply")

        assert symbol.name == "multiply"
        assert symbol.location.start_line == 6

    def test_resolve_not_found(self) -> None:
        """Test error when symbol not found."""
        source = "def foo(): pass"
        resolver = SymbolResolver(source, Path("test.py"))

        with pytest.raises(SymbolNotFoundError) as exc_info:
            resolver.resolve("bar")
        assert "bar" in str(exc_info.value)


class TestPatchApplier:
    """Tests for patch application."""

    def test_apply_add_file(self, tmp_path: Path) -> None:
        """Test adding a new file."""
        patch = """*** Begin Patch
*** Add File: new_file.py
+def hello():
+    print("Hello!")
*** End Patch
"""
        applier = PatchApplier(cwd=tmp_path)
        result = applier.apply(patch, dry_run=False)

        assert result.success
        assert len(result.changes) == 1
        assert result.changes[0].operation == "add"

        # Verify file was created
        new_file = tmp_path / "new_file.py"
        assert new_file.exists()
        assert 'def hello():' in new_file.read_text()

    def test_apply_delete_file(self, tmp_path: Path) -> None:
        """Test deleting a file."""
        # Create file first
        target = tmp_path / "to_delete.py"
        target.write_text("# old content")

        patch = """*** Begin Patch
*** Delete File: to_delete.py
*** End Patch
"""
        applier = PatchApplier(cwd=tmp_path)
        result = applier.apply(patch, dry_run=False)

        assert result.success
        assert not target.exists()

    def test_apply_update_file(self, tmp_path: Path) -> None:
        """Test updating an existing file."""
        # Create file
        target = tmp_path / "update_me.py"
        target.write_text('def greet():\n    print("hi")\n')

        patch = """*** Begin Patch
*** Update File: update_me.py
@@
-    print("hi")
+    print("Hello, World!")
*** End Patch
"""
        applier = PatchApplier(cwd=tmp_path)
        result = applier.apply(patch, dry_run=False)

        assert result.success
        content = target.read_text()
        assert 'print("Hello, World!")' in content
        assert 'print("hi")' not in content

    def test_apply_dry_run(self, tmp_path: Path) -> None:
        """Test dry run doesn't modify files."""
        patch = """*** Begin Patch
*** Add File: should_not_exist.py
+content
*** End Patch
"""
        applier = PatchApplier(cwd=tmp_path)
        result = applier.apply(patch, dry_run=True)

        assert result.success
        assert not (tmp_path / "should_not_exist.py").exists()

    def test_apply_insert_after_symbol(self, tmp_path: Path) -> None:
        """Test inserting after a symbol."""
        # Create file
        target = tmp_path / "module.py"
        target.write_text('''def first():
    pass

def second():
    pass
''')

        patch = """*** Begin Patch
*** Insert After Symbol: module.py::first
+
+def inserted():
+    pass
*** End Patch
"""
        applier = PatchApplier(cwd=tmp_path)
        result = applier.apply(patch, dry_run=False)

        assert result.success
        content = target.read_text()
        # inserted should appear after first but before second
        first_pos = content.find("def first")
        inserted_pos = content.find("def inserted")
        second_pos = content.find("def second")
        assert first_pos < inserted_pos < second_pos

    def test_apply_file_not_found(self, tmp_path: Path) -> None:
        """Test error when updating non-existent file."""
        patch = """*** Begin Patch
*** Update File: does_not_exist.py
@@
-old
+new
*** End Patch
"""
        applier = PatchApplier(cwd=tmp_path)
        result = applier.apply(patch, dry_run=True)

        assert result.status == ApplyStatus.FAILED
        assert any("does not exist" in err for err in result.errors)


class TestApplyPatchConvenience:
    """Tests for the convenience apply_patch function."""

    def test_apply_patch_function(self, tmp_path: Path) -> None:
        """Test the convenience function."""
        from codex_patch import apply_patch

        patch = """*** Begin Patch
*** Add File: quick.py
+# Quick test
*** End Patch
"""
        result = apply_patch(patch, cwd=tmp_path, dry_run=False)

        assert result.success
        assert (tmp_path / "quick.py").exists()


class TestSymbolPath:
    """Tests for SymbolPath parsing."""

    def test_parse_simple(self) -> None:
        """Test parsing simple symbol."""
        path = SymbolPath.parse("function")
        assert path.parts == ("function",)

    def test_parse_nested(self) -> None:
        """Test parsing nested symbol."""
        path = SymbolPath.parse("Class::method")
        assert path.parts == ("Class", "method")

    def test_parse_deeply_nested(self) -> None:
        """Test parsing deeply nested symbol."""
        path = SymbolPath.parse("Module::Class::Inner::method")
        assert path.parts == ("Module", "Class", "Inner", "method")

    def test_str_roundtrip(self) -> None:
        """Test string conversion roundtrip."""
        original = "Class::method"
        path = SymbolPath.parse(original)
        assert str(path) == original
