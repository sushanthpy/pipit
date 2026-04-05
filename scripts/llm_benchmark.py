#!/usr/bin/env python3
"""
LLM Inference Benchmark — Parallel Request Stress Test
=======================================================
Tests an OpenAI-compatible endpoint at various concurrency levels,
measuring latency, throughput, tokens/sec, TTFT, and error rates.
"""

import asyncio
import aiohttp
import json
import time
import statistics
import sys
from dataclasses import dataclass, field
from typing import Optional

# ─── Configuration ───────────────────────────────────────────────────────────
BASE_URL = "http://192.168.1.198:8000/v1/chat/completions"
MODEL = "Qwen/Qwen3.5-35B-A3B"

# Prompts of varying complexity for realistic workload
PROMPTS = [
    {"role": "user", "content": "Reply with exactly: turboquant-ok"},
    {"role": "user", "content": "What is 2+2? Answer in one word."},
    {"role": "user", "content": "Name the capital of France in one word."},
    {"role": "user", "content": "Say hello in Japanese. One word only."},
    {"role": "user", "content": "What color is the sky? One word."},
    {"role": "user", "content": "Reply with exactly: benchmark-pass"},
    {"role": "user", "content": "Is water wet? Yes or no."},
    {"role": "user", "content": "What is the square root of 144?"},
]

MAX_TOKENS = 16
CONCURRENCY_LEVELS = [1, 2, 4, 8, 16, 32, 64]
REQUESTS_PER_LEVEL = 50  # total requests per concurrency level
TIMEOUT_SECONDS = 120
WARMUP_REQUESTS = 3


@dataclass
class RequestResult:
    success: bool
    latency: float  # total time in seconds
    ttft: Optional[float] = None  # time to first token (streaming)
    prompt_tokens: int = 0
    completion_tokens: int = 0
    total_tokens: int = 0
    error: Optional[str] = None
    status_code: Optional[int] = None


@dataclass
class BenchmarkResult:
    concurrency: int
    total_requests: int
    successful: int = 0
    failed: int = 0
    latencies: list = field(default_factory=list)
    ttfts: list = field(default_factory=list)
    total_prompt_tokens: int = 0
    total_completion_tokens: int = 0
    total_tokens: int = 0
    wall_clock_time: float = 0.0
    errors: list = field(default_factory=list)


def percentile(data: list, p: float) -> float:
    """Calculate percentile from sorted data."""
    if not data:
        return 0.0
    sorted_data = sorted(data)
    k = (len(sorted_data) - 1) * (p / 100.0)
    f = int(k)
    c = f + 1
    if c >= len(sorted_data):
        return sorted_data[f]
    return sorted_data[f] + (k - f) * (sorted_data[c] - sorted_data[f])


async def send_request(session: aiohttp.ClientSession, request_id: int) -> RequestResult:
    """Send a single chat completion request and measure timing."""
    prompt = PROMPTS[request_id % len(PROMPTS)]
    payload = {
        "model": MODEL,
        "messages": [prompt],
        "max_tokens": MAX_TOKENS,
        "stream": True,  # stream to measure TTFT
    }

    start_time = time.perf_counter()
    ttft = None

    try:
        async with session.post(
            BASE_URL,
            json=payload,
            timeout=aiohttp.ClientTimeout(total=TIMEOUT_SECONDS),
        ) as resp:
            status = resp.status
            if status != 200:
                body = await resp.text()
                return RequestResult(
                    success=False,
                    latency=time.perf_counter() - start_time,
                    error=f"HTTP {status}: {body[:200]}",
                    status_code=status,
                )

            # Stream response to get TTFT
            prompt_tokens = 0
            completion_tokens = 0
            total_tokens = 0
            first_chunk = True

            async for line in resp.content:
                decoded = line.decode("utf-8").strip()
                if not decoded or not decoded.startswith("data:"):
                    continue
                data_str = decoded[5:].strip()
                if data_str == "[DONE]":
                    break

                if first_chunk:
                    ttft = time.perf_counter() - start_time
                    first_chunk = False

                try:
                    chunk = json.loads(data_str)
                    if "usage" in chunk:
                        usage = chunk["usage"]
                        prompt_tokens = usage.get("prompt_tokens", 0)
                        completion_tokens = usage.get("completion_tokens", 0)
                        total_tokens = usage.get("total_tokens", 0)
                except json.JSONDecodeError:
                    pass

            latency = time.perf_counter() - start_time
            return RequestResult(
                success=True,
                latency=latency,
                ttft=ttft,
                prompt_tokens=prompt_tokens,
                completion_tokens=completion_tokens,
                total_tokens=total_tokens,
                status_code=status,
            )

    except asyncio.TimeoutError:
        return RequestResult(
            success=False,
            latency=time.perf_counter() - start_time,
            error="Timeout",
        )
    except aiohttp.ClientError as e:
        return RequestResult(
            success=False,
            latency=time.perf_counter() - start_time,
            error=str(e),
        )
    except Exception as e:
        return RequestResult(
            success=False,
            latency=time.perf_counter() - start_time,
            error=f"{type(e).__name__}: {e}",
        )


async def run_non_streaming_request(session: aiohttp.ClientSession, request_id: int) -> RequestResult:
    """Fallback: non-streaming request if streaming fails."""
    prompt = PROMPTS[request_id % len(PROMPTS)]
    payload = {
        "model": MODEL,
        "messages": [prompt],
        "max_tokens": MAX_TOKENS,
        "stream": False,
    }

    start_time = time.perf_counter()
    try:
        async with session.post(
            BASE_URL,
            json=payload,
            timeout=aiohttp.ClientTimeout(total=TIMEOUT_SECONDS),
        ) as resp:
            body = await resp.json()
            latency = time.perf_counter() - start_time

            if resp.status != 200:
                return RequestResult(
                    success=False,
                    latency=latency,
                    error=f"HTTP {resp.status}: {json.dumps(body)[:200]}",
                    status_code=resp.status,
                )

            usage = body.get("usage", {})
            return RequestResult(
                success=True,
                latency=latency,
                prompt_tokens=usage.get("prompt_tokens", 0),
                completion_tokens=usage.get("completion_tokens", 0),
                total_tokens=usage.get("total_tokens", 0),
                status_code=resp.status,
            )
    except Exception as e:
        return RequestResult(
            success=False,
            latency=time.perf_counter() - start_time,
            error=f"{type(e).__name__}: {e}",
        )


async def run_concurrency_level(
    concurrency: int, total_requests: int, use_streaming: bool = True
) -> BenchmarkResult:
    """Run benchmark at a specific concurrency level."""
    result = BenchmarkResult(concurrency=concurrency, total_requests=total_requests)
    semaphore = asyncio.Semaphore(concurrency)

    async def bounded_request(session, req_id):
        async with semaphore:
            if use_streaming:
                return await send_request(session, req_id)
            else:
                return await run_non_streaming_request(session, req_id)

    connector = aiohttp.TCPConnector(limit=concurrency + 10, force_close=False)
    async with aiohttp.ClientSession(connector=connector) as session:
        wall_start = time.perf_counter()
        tasks = [bounded_request(session, i) for i in range(total_requests)]
        results = await asyncio.gather(*tasks)
        result.wall_clock_time = time.perf_counter() - wall_start

    for r in results:
        if r.success:
            result.successful += 1
            result.latencies.append(r.latency)
            if r.ttft is not None:
                result.ttfts.append(r.ttft)
            result.total_prompt_tokens += r.prompt_tokens
            result.total_completion_tokens += r.completion_tokens
            result.total_tokens += r.total_tokens
        else:
            result.failed += 1
            result.errors.append(r.error or "Unknown error")

    return result


def print_header():
    """Print benchmark header."""
    print()
    print("=" * 90)
    print("  🚀  LLM INFERENCE BENCHMARK — PARALLEL REQUEST STRESS TEST")
    print("=" * 90)
    print(f"  Endpoint : {BASE_URL}")
    print(f"  Model    : {MODEL}")
    print(f"  Max Tok  : {MAX_TOKENS}")
    print(f"  Requests : {REQUESTS_PER_LEVEL} per concurrency level")
    print(f"  Levels   : {CONCURRENCY_LEVELS}")
    print("=" * 90)
    print()


def print_result(r: BenchmarkResult):
    """Print results for one concurrency level."""
    print(f"\n{'─' * 90}")
    print(f"  ⚡ CONCURRENCY = {r.concurrency}   |   {r.successful}/{r.total_requests} succeeded   |   Wall clock: {r.wall_clock_time:.2f}s")
    print(f"{'─' * 90}")

    if not r.latencies:
        print("  ❌ ALL REQUESTS FAILED")
        if r.errors:
            unique_errors = list(set(r.errors))[:5]
            for e in unique_errors:
                print(f"     • {e}")
        return

    avg_lat = statistics.mean(r.latencies)
    med_lat = statistics.median(r.latencies)
    min_lat = min(r.latencies)
    max_lat = max(r.latencies)
    p95_lat = percentile(r.latencies, 95)
    p99_lat = percentile(r.latencies, 99)
    std_lat = statistics.stdev(r.latencies) if len(r.latencies) > 1 else 0.0

    rps = r.successful / r.wall_clock_time if r.wall_clock_time > 0 else 0
    tokens_per_sec = r.total_completion_tokens / r.wall_clock_time if r.wall_clock_time > 0 else 0

    print(f"\n  📊 LATENCY (seconds)")
    print(f"  {'Min':>8s}  {'Avg':>8s}  {'Median':>8s}  {'P95':>8s}  {'P99':>8s}  {'Max':>8s}  {'StdDev':>8s}")
    print(f"  {min_lat:8.3f}  {avg_lat:8.3f}  {med_lat:8.3f}  {p95_lat:8.3f}  {p99_lat:8.3f}  {max_lat:8.3f}  {std_lat:8.3f}")

    if r.ttfts:
        avg_ttft = statistics.mean(r.ttfts)
        med_ttft = statistics.median(r.ttfts)
        min_ttft = min(r.ttfts)
        max_ttft = max(r.ttfts)
        p95_ttft = percentile(r.ttfts, 95)
        print(f"\n  ⏱️  TIME TO FIRST TOKEN (seconds)")
        print(f"  {'Min':>8s}  {'Avg':>8s}  {'Median':>8s}  {'P95':>8s}  {'Max':>8s}")
        print(f"  {min_ttft:8.3f}  {avg_ttft:8.3f}  {med_ttft:8.3f}  {p95_ttft:8.3f}  {max_ttft:8.3f}")

    print(f"\n  🔄 THROUGHPUT")
    print(f"  Requests/sec     : {rps:.2f}")
    print(f"  Tokens/sec (out) : {tokens_per_sec:.1f}")
    if r.total_tokens > 0:
        print(f"  Prompt tokens    : {r.total_prompt_tokens}")
        print(f"  Completion tokens: {r.total_completion_tokens}")
        print(f"  Total tokens     : {r.total_tokens}")

    if r.failed > 0:
        print(f"\n  ⚠️  ERRORS: {r.failed} failed requests")
        unique_errors = list(set(r.errors))[:5]
        for e in unique_errors:
            count = r.errors.count(e)
            print(f"     • [{count}x] {e[:120]}")


def print_summary_table(all_results: list[BenchmarkResult]):
    """Print a comparative summary table."""
    print(f"\n\n{'=' * 90}")
    print("  📈  SUMMARY — ALL CONCURRENCY LEVELS")
    print(f"{'=' * 90}")
    print()
    print(f"  {'Conc':>5s}  {'OK':>4s}  {'Fail':>4s}  {'RPS':>7s}  {'Tok/s':>7s}  "
          f"{'Avg(s)':>7s}  {'Med(s)':>7s}  {'P95(s)':>7s}  {'P99(s)':>7s}  "
          f"{'TTFT_avg':>8s}  {'Wall(s)':>7s}")
    print(f"  {'─' * 84}")

    for r in all_results:
        if not r.latencies:
            print(f"  {r.concurrency:5d}  {r.successful:4d}  {r.failed:4d}  {'—':>7s}  {'—':>7s}  "
                  f"{'—':>7s}  {'—':>7s}  {'—':>7s}  {'—':>7s}  {'—':>8s}  {r.wall_clock_time:7.2f}")
            continue

        rps = r.successful / r.wall_clock_time if r.wall_clock_time > 0 else 0
        tps = r.total_completion_tokens / r.wall_clock_time if r.wall_clock_time > 0 else 0
        avg_l = statistics.mean(r.latencies)
        med_l = statistics.median(r.latencies)
        p95_l = percentile(r.latencies, 95)
        p99_l = percentile(r.latencies, 99)
        ttft_avg = statistics.mean(r.ttfts) if r.ttfts else 0

        print(f"  {r.concurrency:5d}  {r.successful:4d}  {r.failed:4d}  {rps:7.2f}  {tps:7.1f}  "
              f"{avg_l:7.3f}  {med_l:7.3f}  {p95_l:7.3f}  {p99_l:7.3f}  "
              f"{ttft_avg:8.3f}  {r.wall_clock_time:7.2f}")

    print()

    # Find peak RPS
    best = max(all_results, key=lambda r: (r.successful / r.wall_clock_time) if r.wall_clock_time > 0 and r.latencies else 0)
    if best.latencies:
        peak_rps = best.successful / best.wall_clock_time
        peak_tps = best.total_completion_tokens / best.wall_clock_time if best.wall_clock_time > 0 else 0
        print(f"  🏆  Peak throughput: {peak_rps:.2f} req/s at concurrency={best.concurrency}")
        print(f"  🏆  Peak token rate: {peak_tps:.1f} tok/s at concurrency={best.concurrency}")

    # Optimal latency
    best_lat = min(all_results, key=lambda r: statistics.mean(r.latencies) if r.latencies else float('inf'))
    if best_lat.latencies:
        print(f"  ⚡  Lowest avg latency: {statistics.mean(best_lat.latencies):.3f}s at concurrency={best_lat.concurrency}")

    print(f"\n{'=' * 90}\n")


async def warmup(use_streaming: bool):
    """Send warmup requests to prime the model."""
    print("  🔥 Warming up...")
    connector = aiohttp.TCPConnector(limit=5)
    async with aiohttp.ClientSession(connector=connector) as session:
        tasks = []
        for i in range(WARMUP_REQUESTS):
            if use_streaming:
                tasks.append(send_request(session, i))
            else:
                tasks.append(run_non_streaming_request(session, i))
        results = await asyncio.gather(*tasks)
        ok = sum(1 for r in results if r.success)
        print(f"  ✅ Warmup complete: {ok}/{WARMUP_REQUESTS} succeeded")
        if ok == 0:
            print("  ❌ Warmup failed — check endpoint connectivity")
            return False
    return True


async def main():
    print_header()

    # Quick connectivity check
    print("  🔍 Testing connectivity...")
    connector = aiohttp.TCPConnector(limit=2)
    try:
        async with aiohttp.ClientSession(connector=connector) as session:
            result = await run_non_streaming_request(session, 0)
            if result.success:
                print(f"  ✅ Connection OK — latency: {result.latency:.3f}s")
                use_streaming = True
                # Test streaming
                result2 = await send_request(session, 0)
                if result2.success:
                    print(f"  ✅ Streaming OK — TTFT: {result2.ttft:.3f}s" if result2.ttft else "  ✅ Streaming OK")
                else:
                    print(f"  ⚠️  Streaming failed, falling back to non-streaming: {result2.error}")
                    use_streaming = False
            else:
                print(f"  ❌ Connection FAILED: {result.error}")
                sys.exit(1)
    except Exception as e:
        print(f"  ❌ Cannot reach endpoint: {e}")
        sys.exit(1)

    # Warmup
    if not await warmup(use_streaming):
        sys.exit(1)

    # Run benchmarks
    all_results = []
    for level in CONCURRENCY_LEVELS:
        reqs = REQUESTS_PER_LEVEL
        print(f"\n  🏃 Running {reqs} requests at concurrency={level}...")
        result = await run_concurrency_level(level, reqs, use_streaming)
        print_result(result)
        all_results.append(result)

        # Stop if all requests fail at this level
        if result.failed == result.total_requests:
            print(f"\n  ⛔ All requests failed at concurrency={level}. Stopping benchmark.")
            break

        # Brief cooldown between levels
        await asyncio.sleep(1)

    # Summary
    print_summary_table(all_results)


if __name__ == "__main__":
    asyncio.run(main())
