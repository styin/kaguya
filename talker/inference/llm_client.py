"""inference/llm_client.py — Async HTTP client to an OpenAI-compatible LLM server.

Domain role: Streams token completions from the local LLM server (llama.cpp,
LM Studio, or any OpenAI-compatible backend) and triggers KV cache prefill
between turns.

[DECISION] HTTP client, not gRPC. llama.cpp/LM Studio speak OpenAI-compatible
HTTP. Overhead is ~0.1ms on localhost — negligible.

TODO: Migrate from /v1/completions (raw prompt) to /v1/chat/completions
(structured messages[]) when prompt_formatter returns messages instead of
a string. See prompt_formatter.py module docstring.
"""

import asyncio
import json
import logging
from collections.abc import AsyncIterator

import httpx

from config import TalkerConfig

logger = logging.getLogger(__name__)


class LLMClient:
    """Async streaming client for OpenAI-compatible /v1/completions.

    Lifecycle:
        client = LLMClient(config)
        async for token in client.stream_completion(prompt, cancel_event):
            ...
        await client.prefill(prompt)
        await client.close()
    """

    def __init__(self, config: TalkerConfig) -> None:
        self._base_url = config.llm_base_url.rstrip("/")
        self._max_retries = config.llm_max_retries
        self._retry_delay = config.llm_retry_delay
        self._max_tokens = config.llm_max_tokens
        self._timeout = config.llm_timeout
        self._client = httpx.AsyncClient(base_url=self._base_url, timeout=self._timeout)

    async def close(self) -> None:
        await self._client.aclose()

    async def stream_completion(
        self,
        prompt: str,
        cancel_event: asyncio.Event | None = None,
    ) -> AsyncIterator[str]:
        """POST to /v1/completions with streaming, yield token strings.

        Args:
            prompt: The full prompt string to send to the LLM.
            cancel_event: If set, the stream is aborted immediately.
                Used by PREPARE to interrupt mid-generation during barge-in.

        Retries up to max_retries on connection/server errors before yielding
        any tokens. Once tokens have been yielded, errors are not retried
        (to avoid duplicate output).
        """
        payload = {
            "prompt": prompt,
            "stream": True,
            "max_tokens": self._max_tokens,  # OpenAI-compatible
            "n_predict": self._max_tokens,  # llama.cpp native
            "cache_prompt": True,  # llama.cpp KV cache reuse (ignored by others)
        }
        last_exc: Exception | None = None
        has_yielded = False

        for attempt in range(1, self._max_retries + 1):
            try:
                async with self._client.stream(
                    "POST", "/v1/completions", json=payload
                ) as response:
                    response.raise_for_status()
                    async for token in _strip_think_blocks(
                        _parse_sse_cancellable(response, cancel_event)
                    ):
                        has_yielded = True
                        yield token
                    return  # stream completed successfully
            except (
                httpx.ConnectError,
                httpx.TimeoutException,
                httpx.HTTPStatusError,
            ) as exc:
                # Don't retry if we've already yielded tokens — retrying
                # would re-send the same prompt and produce duplicate output.
                if has_yielded:
                    logger.error("LLM stream failed after yielding tokens: %s", exc)
                    return
                last_exc = exc
                logger.warning(
                    "LLM server request failed (attempt %d/%d): %s",
                    attempt,
                    self._max_retries,
                    exc,
                )
                if attempt < self._max_retries:
                    await asyncio.sleep(self._retry_delay * attempt)

        raise ConnectionError(
            f"LLM server unreachable after {self._max_retries} attempts"
        ) from last_exc

    async def prefill(self, prompt: str) -> None:
        """Send a prefill-only request (n_predict=0) to warm the KV cache.

        Used by PrefillCache RPC between turns. Errors are logged but not fatal
        — a failed prefill just means slightly higher first-token latency.
        """
        payload = {
            "prompt": prompt,
            "max_tokens": 0,  # OpenAI-compatible
            "n_predict": 0,  # llama.cpp native
            "cache_prompt": True,
        }
        try:
            response = await self._client.post("/v1/completions", json=payload)
            response.raise_for_status()
        except (
            httpx.ConnectError,
            httpx.TimeoutException,
            httpx.HTTPStatusError,
        ) as exc:
            logger.warning("Prefill request failed (non-fatal): %s", exc)


async def _parse_sse_cancellable(
    response: httpx.Response,
    cancel_event: asyncio.Event | None = None,
) -> AsyncIterator[str]:
    """Parse Server-Sent Events with cancellation support.

    Expected format (llama.cpp / LM Studio):
        data: {"content": "Hello", "stop": false}
        data: {"content": "", "stop": true}
        data: [DONE]

    If cancel_event is set, the iterator returns immediately —
    no waiting for the next token.
    """
    line_iter = response.aiter_lines().__aiter__()

    while True:
        # Race the next SSE line against the cancel event.
        if cancel_event is not None and cancel_event.is_set():
            return

        try:
            if cancel_event is not None:
                # Wait for either the next line or cancellation.
                line_task = asyncio.ensure_future(_anext(line_iter))
                cancel_task = asyncio.ensure_future(cancel_event.wait())
                done, pending = await asyncio.wait(
                    {line_task, cancel_task},
                    return_when=asyncio.FIRST_COMPLETED,
                )
                for task in pending:
                    task.cancel()

                if cancel_task in done:
                    line_task.cancel()
                    return

                line = line_task.result()
            else:
                line = await _anext(line_iter)
        except StopAsyncIteration:
            return

        if not line.startswith("data: "):
            continue
        data = line[6:]  # strip "data: " prefix
        if data == "[DONE]":
            return
        try:
            chunk = json.loads(data)
        except json.JSONDecodeError:
            logger.debug("Malformed SSE JSON, skipping: %s", data[:100])
            continue
        # OpenAI-compatible /v1/completions format:
        # {"choices": [{"text": "token", "finish_reason": null|"stop"}]}
        choices = chunk.get("choices")
        if not choices:
            continue
        choice = choices[0]
        text = choice.get("text", "")
        if text:
            yield text
        if choice.get("finish_reason") is not None:
            return


async def _strip_think_blocks(tokens: AsyncIterator[str]) -> AsyncIterator[str]:
    """Filter out tokens inside <think>...</think> blocks.

    Some models (e.g., Qwen3.5) emit thinking blocks before or during speech.
    This strips them at the token level so downstream consumers (sentence
    detector, soul container) only see spoken content.

    Handles partial tags across token boundaries (e.g., "<" then "think>")
    and nested/repeated <think> tags.
    """
    in_think = False
    buf = ""

    async for token in tokens:
        buf += token

        while buf:
            if in_think:
                # Look for closing tag.
                end = buf.find("</think>")
                if end != -1:
                    # Skip everything up to and including </think>.
                    buf = buf[end + 8 :]
                    in_think = False
                    continue
                # Might have a partial "</think" at the end — keep it buffered.
                if buf.endswith("<") or any(
                    buf.endswith("</think"[:i]) for i in range(2, 8)
                ):
                    break
                # No closing tag and no partial — discard everything.
                buf = ""
                break
            else:
                # Look for opening tag.
                start = buf.find("<think>")
                if start != -1:
                    # Yield text before the tag.
                    before = buf[:start]
                    if before:
                        yield before
                    buf = buf[start + 7 :]
                    in_think = True
                    continue
                # Check for partial "<think" at the end.
                if buf.endswith("<") or any(
                    buf.endswith("<think"[:i]) for i in range(2, 7)
                ):
                    # Yield everything except the potential partial tag.
                    safe = buf[: -(len(buf) - buf.rfind("<"))]
                    if safe:
                        yield safe
                    buf = buf[len(safe) :]
                    break
                # No tag at all — yield everything.
                yield buf
                buf = ""
                break

    # Flush remaining buffer (not inside a think block).
    if buf and not in_think:
        yield buf


async def _anext(aiter: AsyncIterator) -> str:
    """Wrapper for __anext__ to make it awaitable for asyncio.wait."""
    return await aiter.__anext__()
