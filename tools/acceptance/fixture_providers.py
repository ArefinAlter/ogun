"""The wire formats of the other model providers, for the acceptance run (loopback only).

Each follows what the provider documents: Anthropic Messages (SSE `message_start`, `content_block_*`, `message_delta`,
`message_stop`, header `x-api-key` and `anthropic-version`), Gemini `streamGenerateContent?alt=sse` (SSE chunks of
`candidates[].content.parts`, `functionCall`, `usageMetadata`, header `x-goog-api-key`) and Ollama `/api/chat` (NDJSON,
no key). Like the OpenAI-compatible mock they answer "list workflows" with a call of `workflows__list`, turn a tool
result into a sentence, and record what they were sent (never the key).
"""

import json


def other_chat(handler, kind, key, chat, lock):
    raw = handler.body()
    body = json.loads(raw or b"{}")
    if kind == "anthropic":
        ok = handler.headers.get("x-api-key") == key and bool(handler.headers.get("anthropic-version"))
        msgs = body.get("messages", [])
        last = msgs[-1] if msgs else {}
        blocks = last.get("content") if isinstance(last.get("content"), list) else []
        tool_result = any(b.get("type") == "tool_result" for b in blocks)

        def is_result(m):
            return isinstance(m.get("content"), list) and any(b.get("type") == "tool_result" for b in m["content"])

        user_text = ""
        for m in reversed(msgs):
            if m.get("role") == "user" and not is_result(m):
                c = m.get("content")
                user_text = c if isinstance(c, str) else " ".join(b.get("text", "") for b in c if b.get("type") == "text")
                break
        tools = [t["name"] for t in body.get("tools", [])]
        result_text = " ".join(str(b.get("content", ""))[:160] for b in blocks if b.get("type") == "tool_result")
    elif kind == "google":
        ok = handler.headers.get("x-goog-api-key") == key
        contents = body.get("contents", [])
        last = contents[-1] if contents else {}
        parts = last.get("parts", [])
        tool_result = any("functionResponse" in p for p in parts)
        user_text = ""
        for c in reversed(contents):
            if c.get("role") == "user" and any("text" in p for p in c.get("parts", [])):
                user_text = " ".join(p.get("text", "") for p in c["parts"])
                break
        tools = [d["name"] for t in body.get("tools", []) for d in t.get("functionDeclarations", [])]
        result_text = " ".join(json.dumps(p["functionResponse"].get("response", {}))[:160] for p in parts if "functionResponse" in p)
    else:
        ok = True
        msgs = body.get("messages", [])
        last = msgs[-1] if msgs else {}
        tool_result = last.get("role") == "tool"
        user_text = next((m.get("content", "") for m in reversed(msgs) if m.get("role") == "user"), "")
        tools = [t["function"]["name"] for t in body.get("tools", [])]
        result_text = str(last.get("content", ""))[:160] if tool_result else ""
    with lock:
        chat.append({
            "provider": kind, "authorized": ok, "model": body.get("model"), "tools": tools,
            "has_key_in_body": key.encode() in raw, "user_text": user_text[:120],
        })
    if not ok:
        return handler.reply(401, json.dumps({"error": {"type": "authentication_error", "message": "invalid key"}}))
    call = (not tool_result) and "list workflows" in user_text.lower() and "workflows__list" in tools
    text = ("Here is what I found: " + result_text) if tool_result else "I can list your workflows."
    nl = chr(10)
    if kind == "anthropic":
        ev = [("message_start", {"type": "message_start", "message": {"id": "msg_1", "role": "assistant", "content": [], "usage": {"input_tokens": 77, "output_tokens": 1}}})]
        if call:
            ev += [
                ("content_block_start", {"type": "content_block_start", "index": 0, "content_block": {"type": "tool_use", "id": "toolu_1", "name": "workflows__list", "input": {}}}),
                ("content_block_delta", {"type": "content_block_delta", "index": 0, "delta": {"type": "input_json_delta", "partial_json": "{}"}}),
                ("content_block_stop", {"type": "content_block_stop", "index": 0}),
                ("message_delta", {"type": "message_delta", "delta": {"stop_reason": "tool_use"}, "usage": {"output_tokens": 14}}),
            ]
        else:
            ev.append(("content_block_start", {"type": "content_block_start", "index": 0, "content_block": {"type": "text", "text": ""}}))
            for i in range(0, len(text), 24):
                ev.append(("content_block_delta", {"type": "content_block_delta", "index": 0, "delta": {"type": "text_delta", "text": text[i:i + 24]}}))
            ev += [
                ("content_block_stop", {"type": "content_block_stop", "index": 0}),
                ("message_delta", {"type": "message_delta", "delta": {"stop_reason": "end_turn"}, "usage": {"output_tokens": 21}}),
            ]
        ev.append(("message_stop", {"type": "message_stop"}))
        out = "".join(f"event: {n}{nl}data: {json.dumps(d)}{nl}{nl}" for n, d in ev)
        return handler.reply(200, out, "text/event-stream")
    if kind == "google":
        chunks = []
        if call:
            chunks.append({"candidates": [{"content": {"role": "model", "parts": [{"functionCall": {"name": "workflows__list", "args": {}}}]}, "finishReason": "STOP"}], "usageMetadata": {"promptTokenCount": 66, "candidatesTokenCount": 9}})
        else:
            for i in range(0, len(text), 24):
                chunks.append({"candidates": [{"content": {"role": "model", "parts": [{"text": text[i:i + 24]}]}}]})
            chunks.append({"candidates": [{"content": {"role": "model", "parts": [{"text": ""}]}, "finishReason": "STOP"}], "usageMetadata": {"promptTokenCount": 70, "candidatesTokenCount": 19}})
        out = "".join("data: " + json.dumps(c) + nl + nl for c in chunks)
        return handler.reply(200, out, "text/event-stream")
    lines = []
    if call:
        lines.append({"model": body.get("model"), "message": {"role": "assistant", "content": "", "tool_calls": [{"function": {"name": "workflows__list", "arguments": {}}}]}, "done": False})
        lines.append({"model": body.get("model"), "message": {"role": "assistant", "content": ""}, "done": True, "done_reason": "stop", "prompt_eval_count": 55, "eval_count": 8})
    else:
        for i in range(0, len(text), 24):
            lines.append({"model": body.get("model"), "message": {"role": "assistant", "content": text[i:i + 24]}, "done": False})
        lines.append({"model": body.get("model"), "message": {"role": "assistant", "content": ""}, "done": True, "done_reason": "stop", "prompt_eval_count": 58, "eval_count": 17})
    return handler.reply(200, "".join(json.dumps(x) + nl for x in lines), "application/x-ndjson")
