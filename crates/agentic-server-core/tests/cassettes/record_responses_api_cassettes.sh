#!/usr/bin/env bash
# Record flat Responses API cassettes with the existing Python recorder.
# RESPONSES_RECORD_SET=all|openai|gateway; RESPONSES_SCENARIO=all|lifecycle|pagination|edge-cases|conversation|replayed-items|tool-items.
# RESPONSES_STREAM_MODE=nonstreaming|streaming|both (default: nonstreaming).
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
CASSETTES_DIR="${RESPONSES_CASSETTES_DIR:-$SCRIPT_DIR/responses}"
RECORD_SET="${RESPONSES_RECORD_SET:-all}"
SCENARIO="${RESPONSES_SCENARIO:-all}"
STREAM_MODE="${RESPONSES_STREAM_MODE:-nonstreaming}"
OPENAI_MODEL="${OPENAI_MODEL:-gpt-4.1}"
GATEWAY_MODEL="${GATEWAY_MODEL:-$OPENAI_MODEL}"
GATEWAY_URL="${GATEWAY_URL:-http://localhost:9000}"
WORKTREE_PYTHON="$SCRIPT_DIR/../../../../.venv/bin/python"
if [[ -x "$WORKTREE_PYTHON" ]]; then
    PYTHON_BIN="${PYTHON_BIN:-$WORKTREE_PYTHON}"
else
    PYTHON_BIN="${PYTHON_BIN:-python3}"
fi

case "$RECORD_SET" in
    all|openai|gateway) ;;
    *) echo "RESPONSES_RECORD_SET must be all, openai, or gateway" >&2; exit 2 ;;
esac
case "$SCENARIO" in
    all|lifecycle|pagination|edge-cases|conversation|replayed-items|tool-items) ;;
    *) echo "RESPONSES_SCENARIO must be all, lifecycle, pagination, edge-cases, conversation, replayed-items, or tool-items" >&2; exit 2 ;;
esac
case "$STREAM_MODE" in
    nonstreaming|streaming|both) ;;
    *) echo "RESPONSES_STREAM_MODE must be nonstreaming, streaming, or both" >&2; exit 2 ;;
esac
if [[ "$RECORD_SET" != gateway && -z "${OPENAI_API_KEY:-}" ]]; then
    echo "OPENAI_API_KEY is required for OpenAI recordings" >&2
    exit 2
fi
if ! "$PYTHON_BIN" -c 'import click, fastapi, httpx, uvicorn, yaml' 2>/dev/null; then
    echo "Recorder dependencies are missing for $PYTHON_BIN; install recorder-requirements.txt" >&2
    exit 2
fi

mkdir -p "$CASSETTES_DIR"
record() {
    local provider="$1" scenario="$2" model="$3" transport="$4"
    local turns mode destination temporary
    case "$scenario" in
        lifecycle) turns=19; mode=response-lifecycle ;;
        pagination) turns=16; mode=response-pagination ;;
        edge-cases) turns=23; mode=response-edge-cases ;;
        conversation) turns=19; mode=response-conversation ;;
        replayed-items) turns=14; mode=response-replayed-items ;;
        tool-items) turns=40; mode=response-tool-items ;;
    esac
    local suffix="" stream_flag=--no-stream
    if [[ "$transport" == streaming ]]; then suffix="-stream"; stream_flag=--stream; fi
    destination="$CASSETTES_DIR/response-${scenario}${suffix}-${provider}.yaml"
    temporary="${destination}.tmp"
    local backend=()
    if [[ "$provider" == openai ]]; then
        backend=(--openai https://api.openai.com)
    else
        backend=(--gateway "$GATEWAY_URL")
        export GATEWAY_API_KEY="${GATEWAY_API_KEY:-${OPENAI_API_KEY:-}}"
    fi
    echo "Recording $provider/$scenario -> $destination"
    rm -f "$temporary"
    "$PYTHON_BIN" "$SCRIPT_DIR/record_cassette.py" \
        --mode items --items-scenario "$mode" --turns "$turns" \
        --proxy-port "${RECORDER_PROXY_PORT:-7070}" --model "$model" "$stream_flag" "${backend[@]}" --output "$temporary"
    "$PYTHON_BIN" - "$temporary" "$scenario" "$turns" <<'CASSETTE_CHECK'
import sys
from pathlib import Path
from yaml import safe_load

path, scenario, count = Path(sys.argv[1]), sys.argv[2], int(sys.argv[3])
cassette = safe_load(path.read_text())
assert list(cassette) == ["turns"], "cassette must have one flat turns list"
turns = cassette["turns"]
assert len(turns) == count
assert [turn["filename"] for turn in turns] == [f"t{i}" for i in range(1, count + 1)]
for turn in turns:
    assert set(turn) == {"filename", "request", "response"}
    request, response = turn["request"], turn["response"]
    assert {"method", "path", "query_params", "body", "headers"} <= set(request)
    assert {"status_code", "headers"} <= set(response)
    assert "body" in response or "sse" in response
    assert request["path"].startswith(("/v1/responses", "/v1/conversations")), request["path"]
    assert request["headers"].get("authorization", "Bearer ***") == "Bearer ***"
    assert "transport_error" not in response, "upstream transport failed"
    assert isinstance(response["status_code"], int)
first_path = "/v1/conversations" if scenario == "conversation" else "/v1/responses"
assert (turns[0]["request"]["method"], turns[0]["request"]["path"]) == ("POST", first_path)
if scenario == "lifecycle":
    assert sum(turn["request"]["method"] == "DELETE" for turn in turns) == 4
    assert sum(turn["request"]["path"].endswith("/input_items") for turn in turns) == 8
elif scenario == "pagination":
    assert sum(turn["request"]["method"] == "POST" for turn in turns) == 3
    assert {"asc", "desc"} <= {turn["request"]["query_params"].get("order") for turn in turns[2:]}
elif scenario == "edge-cases":
    assert turns[0]["request"]["body"]["store"] is False
    assert turns[7]["request"]["body"]["previous_response_id"] == "resp_missing"
    assert turns[8]["request"]["body"]["store"] is True
    assert sum(turn["request"]["method"] == "DELETE" for turn in turns) == 4
elif scenario == "conversation":
    assert sum(turn["request"]["method"] == "DELETE" for turn in turns) == 3
    assert any(turn["request"]["body"].get("conversation") for turn in turns)
elif scenario == "replayed-items":
    assert sum(turn["request"]["method"] == "POST" for turn in turns) == 5
    assert any(len(turn["request"]["body"].get("input", [])) == 3 for turn in turns)
elif scenario == "tool-items":
    for create, listing in zip(turns[::2], turns[1::2]):
        assert create["request"]["method"] == "POST"
        assert create["request"]["body"]["stream"] is False
        assert listing["request"]["method"] == "GET"
        response_id = create["response"]["body"].get("id", "resp_missing")
        assert listing["request"]["path"] == f"/v1/responses/{response_id}/input_items"
    assert {"web_search_call", "mcp_list_tools", "mcp_call", "code_interpreter_call",
            "shell_call", "shell_call_output", "custom_tool_call", "custom_tool_call_output"} <= {
        item.get("type") for turn in turns[::2] for item in turn["request"]["body"]["input"]}
print(f"Validated {path.name}: {count} flat HTTP turns")
CASSETTE_CHECK
    mv "$temporary" "$destination"
}

for provider in openai gateway; do
    if [[ "$RECORD_SET" != all && "$RECORD_SET" != "$provider" ]]; then continue; fi
    if [[ "$provider" == openai ]]; then model="$OPENAI_MODEL"; else model="$GATEWAY_MODEL"; fi
    for scenario in lifecycle pagination edge-cases conversation replayed-items tool-items; do
        if [[ "$SCENARIO" != all && "$SCENARIO" != "$scenario" ]]; then continue; fi
        # Admission probes stay JSON so failures remain inspectable. Capture
        # tool-items once even when both transports are selected.
        if [[ "$scenario" == tool-items ]]; then
            record "$provider" "$scenario" "$model" nonstreaming
            continue
        fi
        for transport in nonstreaming streaming; do
            if [[ "$STREAM_MODE" == both || "$STREAM_MODE" == "$transport" ]]; then
                record "$provider" "$scenario" "$model" "$transport"
            fi
        done
    done
done
