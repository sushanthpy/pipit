# TUI Markdown Rendering Improvements

Notes on improvements made to pipit's terminal markdown rendering, inspired by
the [markdown-reader](https://github.com/) reference project at
`/Users/sushanth/git-clone/markdown-reader`.

## Scope

All changes are contained in the `pipit-io` crate:

- `crates/pipit-io/src/app.rs` — main `render_markdown_lines` pipeline
- `crates/pipit-io/src/math.rs` — new module for LaTeX → Unicode math
- `crates/pipit-io/src/lib.rs` — module registration

No public API changes. Downstream crates unaffected.

---

## 1. Fair-Share Table Rendering

**File:** `crates/pipit-io/src/app.rs` (function `render_table_block`)

### Before
- Linear-scale column widths: `scale = available / total_width` applied
  uniformly, so a long column starved narrow ones.
- ASCII-ish separators using `─┼─` between columns and nothing on the outside.
- No visual distinction between header / body / footer.

### After
- New helper `fair_share_widths(naturals, col_count, target)`:
  - Each column guaranteed a minimum of 1–6 chars (`n.clamp(1, 6)`).
  - Remaining width distributed proportionally to each column's *excess over
    the minimum*, so narrow columns keep their full content and wide columns
    absorb the slack.
  - Falls back to equal-ish shares if even the minimums don't fit.
- Proper box-drawing borders:
  - Top:    `┌─┬─┐`
  - Middle: `├─┼─┤` (between header and body)
  - Bottom: `└─┴─┘`
  - Cell row: `│ cell │ cell │`
- Subtle border color `Color::Rgb(80, 85, 100)` to avoid overpowering content.
- Appends `[ columns truncated to fit width ]` hint when width clamped.
- Extracted helpers: `table_border_line`, `render_table_cell_row`.

---

## 2. LaTeX → Unicode Math

**File:** `crates/pipit-io/src/math.rs` *(new)*

New module `pipit_io::math` with a single public function:

```rust
pub fn latex_to_unicode(input: &str) -> String
```

### Supported conversions

| Category       | Examples                                                      |
|----------------|---------------------------------------------------------------|
| Greek (lower)  | `\alpha → α`, `\beta → β`, `\pi → π`, `\omega → ω`            |
| Greek (upper)  | `\Gamma → Γ`, `\Delta → Δ`, `\Sigma → Σ`, `\Omega → Ω`        |
| Operators      | `\sum → ∑`, `\prod → ∏`, `\int → ∫`, `\partial → ∂`           |
| Relations      | `\leq → ≤`, `\geq → ≥`, `\neq → ≠`, `\approx → ≈`, `\equiv → ≡` |
| Arrows         | `\to → →`, `\mapsto → ↦`, `\Rightarrow → ⇒`, `\iff → ⟺`       |
| Logic          | `\forall → ∀`, `\exists → ∃`, `\land → ∧`, `\lor → ∨`         |
| Sets           | `\in → ∈`, `\cup → ∪`, `\cap → ∩`, `\subset → ⊂`              |
| Misc           | `\infty → ∞`, `\nabla → ∇`, `\hbar → ℏ`, `\pm → ±`            |
| Superscripts   | `x^2 → x²`, `x^{2n} → x²ⁿ`, `e^{i\pi} → eⁱπ` (recursive)      |
| Subscripts     | `a_1 → a₁`, `a_{ij} → aᵢⱼ`                                    |
| Fractions      | `\frac{a}{b} → (a)/(b)`                                       |
| Square roots   | `\sqrt{x} → √(x)`, `\sqrt{x^2+y^2} → √(x²+y²)`                |
| Text           | `\text{foo} → foo` (passthrough)                              |

### Wiring

`parse_inline_spans` in `app.rs` now routes pulldown-cmark events through the
new module:

- `Event::InlineMath(math)` → `latex_to_unicode(&math)` rendered in italic.
- `Event::DisplayMath(math)` → wrapped in `⟨ … ⟩` with a soft blue tint.

### Tests

10 unit tests in `math.rs` — all passing:
`test_greek_letters`, `test_operators`, `test_fractions`, `test_sqrt`,
`test_superscripts`, `test_subscripts`, `test_arrows`, `test_logic`,
`test_passthrough`, `test_unknown_command`.

---

## 3. Task List Checkboxes

**File:** `crates/pipit-io/src/app.rs` (bullet list branch, ~line 3066)

The line-based list scanner now inspects the body after `- `:

| Markdown source | Rendered       | Color        |
|-----------------|----------------|--------------|
| `- [ ] task`    | `☐ task`       | `DarkGray`   |
| `- [x] task`    | `☑ task`       | `Green`      |
| `- [X] task`    | `☑ task`       | `Green`      |
| `- regular`     | `• regular`    | `Cyan` (unchanged) |

Continuation wrapping uses a 5-space indent to align with the content after the
checkbox glyph.

---

## 4. Heading Differentiation

**File:** `crates/pipit-io/src/app.rs` (heading branches, ~line 2942)

### Before
H1 = Yellow (underlined with `━` dashes), H2 = Yellow (`◆` prefix),
H3 = Cyan. Yellow-on-yellow made H1 and H2 hard to distinguish.

### After

| Level | Color     | Style                  | Prefix                |
|-------|-----------|------------------------|-----------------------|
| H1    | Cyan      | Bold                   | ` ━━ {heading} ` + underline bar |
| H2    | Blue      | Bold                   | ` ◆ ` prefix          |
| H3    | Magenta   | Bold                   | 3-space indent        |
| H4    | Gray      | Bold + Italic          | 3-space indent        |

H4 rendering is new — previously `#### ` headings fell through to the paragraph
path with no visual treatment.

---

## Verification

```bash
cargo test -p pipit-io --lib   # 118/118 pass (10 new math tests)
cargo check --workspace        # clean
```

Existing behavior preserved:
- `content_matches` / `content_active_match` search highlighting still applied
  to every line including new table borders.
- Turn-cell left-border (`│`) logic in `push_rendered_line` unchanged.
- Code-block highlighting, diff rendering, activity markers untouched.
