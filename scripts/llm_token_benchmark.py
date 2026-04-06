#!/usr/bin/env python3
"""
LLM Token Rate Benchmark
=========================
Measures actual token throughput (input + output tokens/sec) using
non-streaming requests for accurate usage stats, and streaming for
per-token timing.
"""

import asyncio
import aiohttp
import json
import time
import statistics
import sys
from dataclasses import dataclass, field

# ─── Configuration ───────────────────────────────────────────────────────────
BASE_URL = "http://192.168.1.198:8000/v1/chat/completions"
MODEL = "Qwen/Qwen3.5-35B-A3B-FP8"

# Prompts designed to generate MORE output tokens for accurate measurement
PROMPTS = [
    "List the first 10 prime numbers, one per line.",
    "Write a haiku about the ocean, then explain it.",
    "Count from 1 to 20, comma separated.",
    "Name all 7 continents and their largest countries.",
    "Explain what gravity is in exactly 3 sentences.",
    "Write the alphabet backwards, space separated.",
    "List 5 programming languages and one feature each.",
    "What are the 4 seasons? One sentence about each.",
]

MAX_TOKENS = 256  # allow longer generation for better token measurement
CONCURRENCY_LEVELS = [1, 2, 4, 8, 16, 32, 64]
REQUESTS_PER_LEVEL = 30


@dataclass
class RequestResult:
    success: bool
    latency: float
    prompt_tokens: int = 0
    completion_tokens: int = 0
    total_tokens: int = 0
    ttft: float = 0.0  # time to first token (streaming only)
    token_times: list = field(default_factory=list)  # per-token timestamps
    error: str = ""


@dataclass
class LevelResult:
    concurrency: int
    total_requests: int
    successful: int = 0
    failed: int = 0
    latencies: list = field(default_factory=list)
    ttfts: list = field(default_factory=list)
    total_prompt_tokens: int = 0
    total_completion_tokens: int = 0
    total_all_tokens: int = 0
    wall_clock: float = 0.0
    errors: list = field(default_factory=list)
    itls: list = field(default_factory=list)  # inter-token latencies


def pct(data, p):
    if not data:
        return 0.0
    s = sorted(data)
    k = (len(s) - 1) * (p / 100.0)
    f = int(k)
    c = min(f + 1, len(s) - 1)
    return s[f] + (k - f) * (s[c] - s[f])


async def streaming_request(session, req_id):
    """Streaming request: measures TTFT and inter-token latency."""
    prompt = PROMPTS[req_id % len(PROMPTS)]
    payload = {
        "model": MODEL,
        "messages": [{"role": "user", "content": prompt}],
        "max_tokens": MAX_TOKENS,
        "stream": True,
        "stream_options": {"include_usage": True},
    }

    start = time.perf_counter()
    ttft = None
    token_times = []
    prompt_tokens = 0
    completion_tokens = 0
    total_tokens = 0

    try:
        async with session.post(
            BASE_URL, json=payload,
            timeout=aiohttp.ClientTimeout(total=120),
        ) as resp:
            if resp.status != 200:
                body = await resp.text()
                return RequestResult(False, time.perf_counter() - start, error=f"HTTP {resp.status}")

            async for line in resp.content:
                decoded = line.decode("utf-8").strip()
                if not decoded or not decoded.startswith("data:"):
                    continue
                data_str = decoded[5:].strip()
                if data_str == "[DONE]":
                    break
                try:
                    chunk = json.loads(data_str)
                    # Check for usage in the chunk
                    if "usage" in chunk and chunk["usage"]:
                        u = chunk["usage"]
                        prompt_tokens = u.get("prompt_tokens", prompt_tokens)
                        completion_tokens = u.get("completion_tokens", completion_tokens)
                        total_tokens = u.get("total_tokens", total_tokens)

                    choices = chunk.get("choices", [])
                    if choices:
                        delta = choices[0].get("delta", {})
                        content = delta.get("content", "")
                        if content:
                            now = time.perf_counter()
                            if ttft is None:
                                ttft = now - start
                            token_times.append(now)
                except json.JSONDecodeError:
                    pass

            latency = time.perf_counter() - start
            return RequestResult(
                success=True, latency=latency,
                prompt_tokens=prompt_tokens,
                completion_tokens=completion_tokens,
                total_tokens=total_tokens,
                ttft=ttft or 0.0,
                token_times=token_times,
            )
    except Exception as e:
        return RequestResult(False, time.perf_counter() - start, error=str(e))


async def non_streaming_request(session, req_id):
    """Non-streaming: gets accurate token counts from usage field."""
    prompt = PROMPTS[req_id % len(PROMPTS)]
    payload = {
        "model": MODEL,
        "messages": [{"role": "user", "content": prompt}],
        "max_tokens": MAX_TOKENS,
        "stream": False,
    }
    start = time.perf_counter()
    try:
        async with session.post(
            BASE_URL, json=payload,
            timeout=aiohttp.ClientTimeout(total=120),
        ) as resp:
            body = await resp.json()
            latency = time.perf_counter() - start
            if resp.status != 200:
                return RequestResult(False, latency, error=f"HTTP {resp.status}")
            u = body.get("usage", {})
            return RequestResult(
                True, latency,
                prompt_tokens=u.get("prompt_tokens", 0),
                completion_tokens=u.get("completion_tokens", 0),
                total_tokens=u.get("total_tokens", 0),
            )
    except Exception as e:
        return RequestResult(False, time.perf_counter() - start, error=str(e))


async def run_level(concurrency, total_reqs, use_streaming=True):
    result = LevelResult(concurrency=concurrency, total_requests=total_reqs)
    sem = asyncio.Semaphore(concurrency)

    async def bound(session, i):
        async with sem:
            if use_streaming:
                return await streaming_request(session, i)
            else:
                return await non_streaming_request(session, i)

    conn = aiohttp.TCPConnector(limit=concurrency + 10)
    async with aiohttp.ClientSession(connector=conn) as session:
        wall_start = time.perf_counter()
        tasks = [bound(session, i) for i in range(total_reqs)]
        results = await asyncio.gather(*tasks)
        result.wall_clock = time.perf_counter() - wall_start

    for r in results:
        if r.success:
            result.successful += 1
            result.latencies.append(r.latency)
            result.total_prompt_tokens += r.prompt_tokens
            result.total_completion_tokens += r.completion_tokens
            result.total_all_tokens += r.total_tokens
            if r.ttft > 0:
                result.ttfts.append(r.ttft)
            # Inter-token latencies
            if len(r.token_times) > 1:
                for j in range(1, len(r.token_times)):
                    result.itls.append(r.token_times[j] - r.token_times[j - 1])
        else:
            result.failed += 1
            result.errors.append(r.error)

    return result


def print_level(r):
    if not r.latencies:
        print(f"  ❌ Concurrency {r.concurrency}: ALL FAILED")
        return

    rps = r.successful / r.wall_clock
    out_tps = r.total_completion_tokens / r.wall_clock if r.total_completion_tokens else 0
    in_tps = r.total_prompt_tokens / r.wall_clock if r.total_prompt_tokens else 0
    total_tps = r.total_all_tokens / r.wall_clock if r.total_all_tokens else 0
    avg_out = r.total_completion_tokens / r.successful if r.successful else 0
    avg_in = r.total_prompt_tokens / r.successful if r.successful else 0

    print(f"\n{'─' * 90}")
    print(f"  ⚡ CONCURRENCY = {r.concurrency}   |   {r.successful}/{r.total_requests} OK   |   Wall: {r.wall_clock:.2f}s")
    print(f"{'─' * 90}")

    print(f"\n  🎯 TOKEN COUNTS")
    print(f"  Total prompt tokens    : {r.total_prompt_tokens:,}")
    print(f"  Total completion tokens: {r.total_completion_tokens:,}")
    print(f"  Total all tokens       : {r.total_all_tokens:,}")
    print(f"  Avg prompt/request     : {avg_in:.1f}")
    print(f"  Avg completion/request : {avg_out:.1f}")

    print(f"\n  🚀 THROUGHPUT")
    print(f"  Requests/sec           : {rps:.2f}")
    print(f"  Output tokens/sec      : {out_tps:.1f}")
    print(f"  Input tokens/sec       : {in_tps:.1f}")
    print(f"  Total tokens/sec       : {total_tps:.1f}")

    avg_l = statistics.mean(r.latencies)
    print(f"\n  📊 LATENCY")
    print(f"  {'Min':>8s}  {'Avg':>8s}  {'Median':>8s}  {'P95':>8s}  {'P99':>8s}  {'Max':>8s}")
    print(f"  {min(r.latencies):8.3f}  {avg_l:8.3f}  {statistics.median(r.latencies):8.3f}  "
          f"{pct(r.latencies, 95):8.3f}  {pct(r.latencies, 99):8.3f}  {max(r.latencies):8.3f}")

    if r.ttfts:
        print(f"\n  ⏱️  TTFT: min={min(r.ttfts):.3f}s  avg={statistics.mean(r.ttfts):.3f}s  "
              f"med={statistics.median(r.ttfts):.3f}s  p95={pct(r.ttfts, 95):.3f}s")

    if r.itls:
        print(f"\n  🔬 INTER-TOKEN LATENCY (decode speed)")
        print(f"  Avg: {statistics.mean(r.itls)*1000:.1f}ms  "
              f"Med: {statistics.median(r.itls)*1000:.1f}ms  "
              f"P95: {pct(r.itls, 95)*1000:.1f}ms  "
              f"→ ~{1/statistics.mean(r.itls):.1f} tokens/sec per stream")


async def main():
    print("\n" + "=" * 90)
    print("  🚀  LLM TOKEN RATE BENCHMARK")
    print("=" * 90)
    print(f"  Endpoint  : {BASE_URL}")
    print(f"  Model     : {MODEL}")
    print(f"  Max Tokens: {MAX_TOKENS}")
    print(f"  Requests  : {REQUESTS_PER_LEVEL} per level")
    print(f"  Levels    : {CONCURRENCY_LEVELS}")
    print("=" * 90)

    # Connectivity + feature detection
    print("\n  🔍 Testing connectivity...")
    conn = aiohttp.TCPConnector(limit=2)
    async with aiohttp.ClientSession(connector=conn) as session:
        # Non-streaming test
        r = await non_streaming_request(session, 0)
        if not r.success:
            print(f"  ❌ FAILED: {r.error}")
            sys.exit(1)
        print(f"  ✅ Non-streaming OK — {r.completion_tokens} output tokens in {r.latency:.3f}s")

        # Streaming test  
        r2 = await streaming_request(session, 0)
        use_streaming = r2.success
        if use_streaming:
            src = "streaming" if r2.completion_tokens > 0 else "streaming (no usage in SSE)"
            print(f"  ✅ Streaming OK — TTFT: {r2.ttft:.3f}s, {len(r2.token_times)} chunks, "
                  f"{r2.completion_tokens} completion tokens ({src})")
        else:
            print(f"  ⚠️  Streaming failed, using non-streaming only")

    # If streaming doesn't report usage, we'll do a hybrid approach:
    # streaming for TTFT/ITL, then use non-streaming for token counts
    # Actually let's check: does streaming return usage?
    streaming_has_usage = use_streaming and r2.completion_tokens > 0

    if not streaming_has_usage and use_streaming:
        print("\n  ℹ️  Streaming doesn't return token usage — running NON-STREAMING for token counts")
        print("      + STREAMING for TTFT and inter-token latency\n")

    # Warmup
    print("  🔥 Warming up...")
    conn = aiohttp.TCPConnector(limit=5)
    async with aiohttp.ClientSession(connector=conn) as session:
        tasks = [non_streaming_request(session, i) for i in range(3)]
        await asyncio.gather(*tasks)
    print("  ✅ Warm\n")

    all_results = []

    for level in CONCURRENCY_LEVELS:
        print(f"  🏃 Concurrency={level}...")

        if streaming_has_usage:
            # Streaming gives us everything
            result = await run_level(level, REQUESTS_PER_LEVEL, use_streaming=True)
        else:
            # Run non-streaming for token counts
            result_ns = await run_level(level, REQUESTS_PER_LEVEL, use_streaming=False)

            # Run streaming for TTFT/ITL timing (smaller batch)
            if use_streaming:
                result_st = await run_level(level, min(REQUESTS_PER_LEVEL, 15), use_streaming=True)
                # Merge timing data into non-streaming result
                result_ns.ttfts = result_st.ttfts
                result_ns.itls = result_st.itls

            result = result_ns

        print_level(result)
        all_results.append(result)

        if result.failed == result.total_requests:
            print(f"\n  ⛔ All failed at concurrency={level}. Stopping.")
            break

        await asyncio.sleep(1)

    # ─── Summary Table ─────────────────────────────────────────────────────
    print(f"\n\n{'=' * 100}")
    print(f"  📈  SUMMARY — TOKEN THROUGHPUT ACROSS CONCURRENCY LEVELS")
    print(f"{'=' * 100}\n")

    header = (f"  {'Conc':>5s}  {'OK':>3s}  {'Req/s':>6s}  "
              f"{'Out tok/s':>10s}  {'In tok/s':>10s}  {'Total tok/s':>11s}  "
              f"{'Avg Out':>8s}  {'Avg Lat':>8s}  {'TTFT':>6s}  "
              f"{'ITL(ms)':>8s}  {'Wall':>6s}")
    print(header)
    print(f"  {'─' * 95}")

    for r in all_results:
        if not r.latencies:
            print(f"  {r.concurrency:5d}  {r.successful:3d}  {'—':>6s}  {'—':>10s}  {'—':>10s}  {'—':>11s}  "
                  f"{'—':>8s}  {'—':>8s}  {'—':>6s}  {'—':>8s}  {r.wall_clock:6.1f}")
            continue

        rps = r.successful / r.wall_clock
        out_tps = r.total_completion_tokens / r.wall_clock if r.total_completion_tokens else 0
        in_tps = r.total_prompt_tokens / r.wall_clock if r.total_prompt_tokens else 0
        total_tps = r.total_all_tokens / r.wall_clock if r.total_all_tokens else 0
        avg_out = r.total_completion_tokens / r.successful
        avg_lat = statistics.mean(r.latencies)
        ttft_s = f"{statistics.mean(r.ttfts):.3f}" if r.ttfts else "—"
        itl_s = f"{statistics.mean(r.itls)*1000:.1f}" if r.itls else "—"

        print(f"  {r.concurrency:5d}  {r.successful:3d}  {rps:6.2f}  "
              f"{out_tps:10.1f}  {in_tps:10.1f}  {total_tps:11.1f}  "
              f"{avg_out:8.1f}  {avg_lat:8.3f}  {ttft_s:>6s}  "
              f"{itl_s:>8s}  {r.wall_clock:6.1f}")

    # Peak stats
    best_out = max(all_results, key=lambda r: r.total_completion_tokens / r.wall_clock if r.wall_clock > 0 and r.latencies else 0)
    best_total = max(all_results, key=lambda r: r.total_all_tokens / r.wall_clock if r.wall_clock > 0 and r.latencies else 0)

    if best_out.latencies and best_out.total_completion_tokens:
        print(f"\n  🏆  Peak output token rate: {best_out.total_completion_tokens / best_out.wall_clock:.1f} tok/s at concurrency={best_out.concurrency}")
    if best_total.latencies and best_total.total_all_tokens:
        print(f"  🏆  Peak total token rate : {best_total.total_all_tokens / best_total.wall_clock:.1f} tok/s at concurrency={best_total.concurrency}")

    if all_results and all_results[0].itls:
        avg_itl = statistics.mean(all_results[0].itls)
        print(f"  ⚡  Single-stream decode  : ~{1/avg_itl:.1f} tok/s ({avg_itl*1000:.1f}ms per token)")

    print(f"\n{'=' * 100}\n")


if __name__ == "__main__":
    asyncio.run(main())
