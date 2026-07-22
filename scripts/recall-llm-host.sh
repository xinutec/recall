#!/usr/bin/env bash
# The LLM holder for the whole Mac (see src/recall/llmhost.py).
#
# One process owns the ~4.3 GB of Qwen weights and serves generation on
# 127.0.0.1:8092; recall's summaries/Ask and life's emotion worker are both
# clients. Before this existed each loaded its own copy, and the refine daemon
# never released its one — so a machine that had answered a single question held
# the weights until the daemon restarted.
#
# Idle policy lives here, in one place: five minutes of quiet and the weights go
# back. The reload costs ~60s, paid by work nobody is sitting waiting on.
set -euo pipefail

exec /Users/pippijn/Code/recall/scripts/recall.sh llm-host
