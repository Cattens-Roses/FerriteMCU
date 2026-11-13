#!/usr/bin/env bash
# Regenerate the Windows, macOS, Linux and in-app icons from assets/branding SVGs.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
python "$ROOT/scripts/generate-brand-assets.py"
