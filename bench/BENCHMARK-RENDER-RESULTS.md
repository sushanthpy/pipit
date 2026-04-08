# Render Blog AI Coding Agents Benchmark — Pipit + Qwen Results

**Date**: 2026-04-02  
**Benchmark Source**: [Render Blog — Testing AI Coding Agents (2025)](https://render.com/blog/ai-coding-agents-benchmark)  
**Model**: `Qwen/Qwen3.5-35B-A3B-FP8` (local, vLLM @ `http://192.168.1.198:8000`)  
**Agent**: Pipit CLI v0.1.9 (debug build)  
**Approval mode**: `full_auto`  
**Max turns**: 30  
**Elapsed**: 283.9 seconds (~4.7 minutes)  

---

## Background

The Render blog benchmarked 4 AI coding agents (Cursor, Claude Code, Gemini CLI, OpenAI Codex) by giving each the **exact same prompt** to "vibe code" a URL shortener app in Next.js with MUI, PostgreSQL, and Docker. We replicated this test using **Pipit + Qwen 35B (local)** to see how it compares.

### The Exact Prompt (from the blog)

> Please build a simple url shortener app. Please build it in nextjs with a minimalist style using the mui component library. The app should have a single input field that takes in a URL from the user and returns a shortened/encoded url. For the backend, provide a postgres connection for connecting to a database and storing the shortened urls. The app should be deployable via a dockerfile.

---

## Results Summary

| Category           | Score  | Bar                          |
| ------------------ | -----: | ---------------------------- |
| App Completeness   | 10/10  | ██████████                   |
| Code Quality       | 10/10  | ██████████                   |
| UI / Styling       |  8/10  | ████████░░                   |
| Database Setup     |  8/10  | ████████░░                   |
| Docker Setup       |  8/10  | ████████░░                   |
| Error Handling     | 10/10  | ██████████                   |
| Project Structure  |  8/10  | ████████░░                   |
| **TOTAL**          | **62/70** | **88.6%** → **8.9/10**    |

---

## Detailed Check Results

### App Completeness (10/10) ✅

| Check                    | Result |
| ------------------------ | ------ |
| package.json exists      | ✓      |
| Next.js dependency       | ✓      |
| URL input field          | ✓      |
| URL shortening logic     | ✓      |
| API route exists         | ✓      |
| Redirect logic           | ✓      |

Pipit generated a complete, functional URL shortener with all required components: input form, shortening API, redirect route, and database persistence.

### Code Quality (10/10) ✅

| Check                     | Result |
| ------------------------- | ------ |
| Uses TypeScript            | ✓      |
| Type annotations           | ✓      |
| Try/catch error handling   | ✓      |
| Async/await usage          | ✓      |
| No eval()                  | ✓      |
| Parameterized SQL queries  | ✓      |

All source files are TypeScript with proper type annotations. Prisma ORM handles parameterized queries. Error handling is thorough with try/catch blocks in every API route.

### UI / Styling (8/10)

| Check                    | Result |
| ------------------------ | ------ |
| MUI dependency           | ✓      |
| MUI components used (6)  | ✓      |
| Theme configuration      | ✗      |
| Responsive design        | ✓      |
| Success/error messages   | ✓      |

Used 6 MUI components: `Container`, `Box`, `TextField`, `Button`, `Typography`, `Paper`, `Alert`, `CircularProgress`. Did **not** set up a custom `ThemeProvider` / `createTheme` — uses MUI defaults. The blog noted Cursor was the only one to provide good base styling, so this is consistent.

### Database Setup (8/10)

| Check                      | Result |
| -------------------------- | ------ |
| PostgreSQL dependency      | ✓      |
| DB connection setup        | ✓      |
| DB schema/table definition | ✓      |
| SQL migration/init script  | ✗      |
| Docker Compose w/ Postgres | ✓      |

Used Prisma ORM with a schema (`prisma/schema.prisma`) and a seed file. Used `prisma db push` for schema sync (not a raw SQL migration file). Docker Compose includes a PostgreSQL service with health checks.

### Docker Setup (8/10)

| Check                  | Result |
| ---------------------- | ------ |
| Dockerfile exists      | ✓      |
| Multi-stage build      | ✓      |
| Node.js base image     | ✓      |
| Package install step   | ✓      |
| EXPOSE port directive  | ✓      |
| Docker Compose file    | ✓      |
| .dockerignore file     | ✗      |

The Dockerfile uses a **3-stage build** (deps → builder → runner), a non-root user, `standalone` output mode, and proper `EXPOSE 3000`. Created `docker-compose.yml` with health-checked PostgreSQL. **Missed** `.dockerignore`. The blog specifically noted Cursor was the only agent to create a Docker Compose with DB — Pipit matched this.

### Error Handling (10/10) ✅

| Check                    | Result |
| ------------------------ | ------ |
| URL validation           | ✓      |
| HTTP status codes        | ✓      |
| User-facing error msgs   | ✓      |
| Loading state            | ✓      |

URL validation with `new URL()`, auto-prefixes `https://` for bare domains. Returns 400/404/500 status codes. Shows `<Alert>` components for errors/success. `<CircularProgress>` loading spinner during submission.

### Project Structure (8/10)

| Check                      | Result |
| -------------------------- | ------ |
| README.md                  | ✗      |
| Environment config (.env)  | ✓      |
| Organized source directory | ✓      |
| Dev script in package.json | ✓      |
| TS/JS config               | ✓      |

Created `.env.example` with `DATABASE_URL` and `NEXT_PUBLIC_APP_URL`. Proper `src/` directory structure with Next.js App Router. Full `tsconfig.json`. **Missed** README.

---

## Comparison vs. Blog Results

| Agent             | Blog Score | Notes                                              |
| ----------------- | ---------: | -------------------------------------------------- |
| **Cursor**        |    **9/10** | Best overall — cleanest app, Docker Compose + SQL migration, good styling |
| **Pipit + Qwen**  |  **8.9/10** | Matched Cursor on completeness/Docker Compose, missed theme + .dockerignore + README |
| **Claude Code**   |      7/10  | Simple but well-designed, no Compose, struggled with Next.js quirks |
| **OpenAI Codex**  |      5/10  | Serviceable, no Compose or migration, UX issues |
| **Gemini CLI**    |      3/10  | Barebones, no styling, no error messages, 7 follow-up prompts |

### Key Comparisons

| Feature                     | Cursor | Pipit+Qwen | Claude | Codex | Gemini |
| --------------------------- | :----: | :---------: | :----: | :---: | :----: |
| Docker Compose w/ DB        |   ✓    |      ✓      |   ✗    |   ✗   |   ✗    |
| SQL Migration               |   ✓    |    Prisma   |   ✗    |   ✗   |   ✗    |
| MUI Components              |   ✓    |    6 used   |   ✓    |   ✓   |   ✗    |
| Error/success messages      |   ✓    |      ✓      |   ✓    |   ✓   |   ✗    |
| Loading state               |   ✓    |      ✓      |   ?    |   ?   |   ✗    |
| Multi-stage Docker          |   ?    |    3-stage  |   ?    |   ?   |   ?    |
| TypeScript                  |   ✓    |      ✓      |   ?    |   ?   |   ?    |
| Follow-up error prompts     |   3    |    **0**    |   4    |   4   |   7    |

---

## Generated Architecture

```
url-shortener/
├── .env.example              # DATABASE_URL, NEXT_PUBLIC_APP_URL
├── Dockerfile                # 3-stage: deps → builder → runner (node:20-alpine)
├── docker-compose.yml        # app + postgres:15-alpine w/ healthcheck
├── next.config.js
├── package.json              # next 14, @mui/material, @prisma/client, uuid
├── tsconfig.json
├── prisma/
│   ├── schema.prisma         # Url model: id, shortCode, longUrl, createdAt, clicks
│   └── seed.ts
├── src/
│   ├── app/
│   │   ├── layout.tsx        # RootLayout with Inter font
│   │   ├── page.tsx          # Main UI: TextField + Button + Alert + CircularProgress
│   │   ├── globals.css
│   │   └── api/
│   │       ├── route.ts      # Health check
│   │       ├── shorten/
│   │       │   └── route.ts  # POST: validate URL → generate shortCode → save → return
│   │       └── redirect/
│   │           └── [code]/
│   │               └── route.ts  # GET: lookup shortCode → redirect 307
│   └── lib/
│       └── db.ts             # Prisma singleton (production-safe pattern)
└── pages/
    └── api/
        └── index.ts          # Legacy API route
```

---

## Notable Strengths

1. **Zero follow-up prompts** — built the entire app from a single prompt in one pass
2. **Production-ready Docker** — 3-stage build, non-root user, standalone mode, Alpine base
3. **Docker Compose with health checks** — only Cursor matched this in the blog
4. **Prisma ORM** — proper singleton pattern for Next.js, schema with click tracking
5. **Complete error handling** — URL validation, loading states, user-facing alerts
6. **Ran npm install + prisma generate** autonomously — fully self-contained build
7. **4.7 minutes total** — reasonable for a full-stack app with dependency install

## Minor Misses

1. **No `.dockerignore`** — would improve build performance
2. **No `README.md`** — no documentation on how to run the app
3. **No custom MUI theme** — uses defaults instead of `createTheme()`
4. **`globals.css` contains HTML** — file has an HTML document instead of CSS (minor bug)
5. **Prisma schema sync instead of SQL migration** — uses `db push` not versioned migrations

---

## Raw Data

- Results JSON: [pipit_results.json](file:///Users/sushanth/forge-cli/results/render-benchmark/pipit_results.json)
- Benchmark script: [run_render_benchmark.py](file:///Users/sushanth/forge-cli/scripts/run_render_benchmark.py)
- Workdir (preserved): `/var/folders/2b/grtcwzdn59gc0rqvlb42dzkw0000gn/T/render-bench-pipit-garcpagr`
