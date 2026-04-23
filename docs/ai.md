# AI Integration

gozpak includes AI-powered extensions for automating package maintenance tasks.
All AI features are optional and require a configured backend.

## Backends

### Ollama (default, local)

```sh
export GOZPAK_AI_BACKEND=ollama
export GOZPAK_AI_MODEL=llama3        # optional, default: llama3
export GOZPAK_AI_API_URL=http://localhost:11434/api/generate  # optional
```

Requires [Ollama](https://ollama.ai) running locally.

### llama.cpp (local)

```sh
export GOZPAK_AI_BACKEND=llamacpp
export GOZPAK_AI_API_URL=http://localhost:8080/completion  # optional
```

Requires a [llama.cpp](https://github.com/ggerganov/llama.cpp) server running.

### Claude (Anthropic API)

```sh
export GOZPAK_AI_BACKEND=claude
export GOZPAK_AI_API_KEY=sk-ant-...  # or set ANTHROPIC_API_KEY
export GOZPAK_AI_MODEL=claude-sonnet-4-20250514  # optional
```

### OpenAI

```sh
export GOZPAK_AI_BACKEND=openai
export GOZPAK_AI_API_KEY=sk-...      # or set OPENAI_API_KEY
export GOZPAK_AI_MODEL=gpt-4o        # optional
```

### OAuth (generic bearer token)

For third-party AI services that use OAuth/bearer token authentication:

```sh
export GOZPAK_AI_BACKEND=oauth
export GOZPAK_AI_API_URL=https://ai.example.com/v1/chat
export GOZPAK_AI_OAUTH_TOKEN=<your-token>
export GOZPAK_AI_MODEL=model-name    # optional
```

## Commands

### `gozpak ai-new <name> <version> <source-url>`

Generate a complete package recipe from a source tarball.

1. Downloads and extracts the source
2. Detects the build system (autotools, cmake, meson, cargo, go, make, python)
3. Reads build files, README, and file listing
4. Sends analysis to the AI backend
5. Writes `build`, `depends`, `version`, and `sources` files

```sh
gozpak ai-new curl 8.7.1 https://curl.se/download/curl-8.7.1.tar.xz
# Creates: curl/build, curl/depends, curl/version, curl/sources

# Review the generated files, then:
gozpak checksum curl
gozpak build curl
```

If the AI fails to generate a build script, a sensible fallback is written
based on the detected build system.

### `gozpak ai-fix <name> [log-file]`

Diagnose a package build failure.

Reads the build log (auto-detected from `~/.cache/gozpak/logs/` or manually
specified), the package's build script and depends file, and asks the AI
for a diagnosis with a concrete fix.

```sh
gozpak build curl        # fails
gozpak ai-fix curl       # diagnose from latest log
gozpak ai-fix curl /path/to/log  # diagnose from specific log
```

### `gozpak ai-update <name> [name2 ...]`

Check for upstream version updates.

Reads the package's current version and source URLs, asks the AI if a newer
stable release exists.

```sh
gozpak ai-update curl openssl zlib
```

Note: The AI's knowledge has a cutoff date. For critical updates, verify
against the upstream project's release page.

## Environment variables

| Variable | Default | Description |
|----------|---------|-------------|
| `GOZPAK_AI_BACKEND` | `ollama` | Backend to use |
| `GOZPAK_AI_MODEL` | varies | Model name (backend-specific default) |
| `GOZPAK_AI_API_KEY` | — | API key for claude/openai backends |
| `GOZPAK_AI_API_URL` | varies | Override API endpoint URL |
| `GOZPAK_AI_OAUTH_TOKEN` | — | Bearer token for oauth backend |
| `ANTHROPIC_API_KEY` | — | Fallback for claude backend |
| `OPENAI_API_KEY` | — | Fallback for openai backend |
