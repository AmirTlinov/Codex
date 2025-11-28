.PHONY: start install dev test lint format clean help

# Default Python interpreter
PYTHON ?= python3

# Project paths
SRC_DIR := src
TEST_DIR := tests

help:
	@echo "Codex-py Development Commands:"
	@echo ""
	@echo "  make start       - Run Codex TUI"
	@echo "  make install     - Install package in development mode"
	@echo "  make dev         - Install with dev dependencies"
	@echo "  make test        - Run all tests"
	@echo "  make lint        - Run linter (ruff)"
	@echo "  make format      - Format code (ruff)"
	@echo "  make clean       - Remove build artifacts"
	@echo ""

# Run the TUI application
start: install
	$(PYTHON) -m codex_tui.app

# Install package in development mode
install:
	pip install -e .

# Install with dev dependencies
dev:
	pip install -e ".[dev]"

# Run tests
test:
	$(PYTHON) -m pytest $(TEST_DIR) -v

# Run tests with coverage
test-cov:
	$(PYTHON) -m pytest $(TEST_DIR) -v --cov=$(SRC_DIR) --cov-report=term-missing

# Run linter
lint:
	ruff check $(SRC_DIR) $(TEST_DIR)

# Format code
format:
	ruff check --fix $(SRC_DIR) $(TEST_DIR)
	ruff format $(SRC_DIR) $(TEST_DIR)

# Type checking
typecheck:
	mypy $(SRC_DIR)

# Clean build artifacts
clean:
	rm -rf build/
	rm -rf dist/
	rm -rf *.egg-info/
	rm -rf src/*.egg-info/
	rm -rf .pytest_cache/
	rm -rf .mypy_cache/
	rm -rf .ruff_cache/
	find . -type d -name "__pycache__" -exec rm -rf {} + 2>/dev/null || true
	find . -type f -name "*.pyc" -delete

# Run CLI in exec mode (non-interactive)
exec:
	$(PYTHON) -m codex_core.cli

# Quick development cycle
quick: format lint test
