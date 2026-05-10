#!/usr/bin/env bash
set -euo pipefail

cargo test -p pocopine-analytics
cargo test -p observability-smoke --test analytics_exporter
