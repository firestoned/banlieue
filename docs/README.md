# banlieue Documentation

This directory contains the [MkDocs Material](https://squidfunk.github.io/mkdocs-material/) site for banlieue.

## Quick Start

### Prerequisites

- Python 3.10+
- [Poetry](https://python-poetry.org/) (recommended) or pip

### Setup

```bash
cd docs

# Using Poetry (recommended)
poetry install
poetry run mkdocs serve

# Or using pip
pip install mkdocs mkdocs-material mkdocs-git-revision-date-localized-plugin pymdown-extensions
mkdocs serve
```

The site is served at <http://127.0.0.1:8000> with live reload.

### Build static site

```bash
poetry run mkdocs build       # output: docs/site/
```

Or from the project root:

```bash
make docs                     # build static site
make docs-serve               # live-reload dev server
make docs-clean               # remove docs/site, docs/.venv
```

## Structure

```
docs/
├── mkdocs.yml                # MkDocs configuration
├── pyproject.toml            # Python dependencies (Poetry)
├── .python-version           # Python version pin
├── .gitignore                # Build artifacts ignored
└── src/                      # Documentation source
    ├── index.md              # Homepage
    ├── stylesheets/          # Custom CSS
    ├── javascripts/          # Mermaid init etc.
    ├── reasoning/            # Why banlieue exists (the case for this project)
    ├── concepts/             # Architecture, VirtualMachine, providers
    ├── getting-started/      # Quick start
    └── reference/            # Roadmap, license
```

## Adding a new page

1. Create a new `.md` file under `src/` in the appropriate folder.
2. Add the page to the `nav:` section of `mkdocs.yml`.
3. Run `mkdocs serve` to preview.

## Features

- Material Design theme with dark mode
- Full-text search
- Mermaid diagrams (with zoom/pan)
- Code syntax highlighting + copy button
- Git last-updated timestamps
