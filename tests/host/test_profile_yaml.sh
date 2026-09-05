#!/bin/bash
set -euo pipefail
exec python3 "$(dirname "$0")/test_profile_yaml.py"
