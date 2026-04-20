//! LaTeX → Unicode conversion for inline/display math in markdown.
//!
//! Produces readable Unicode text for common mathematical notation without
//! requiring external rendering. Handles:
//! - Greek letters (`\alpha` → α)
//! - Operators (`\sum`, `\int`, `\leq`, `\to`, …)
//! - Superscripts/subscripts (`x^2` → x², `a_{ij}` → a_ij)
//! - Fractions (`\frac{a}{b}` → (a)/(b))
//! - Square roots (`\sqrt{x}` → √x)
//!
//! Inspired by the markdown-reader project's pure-Rust approach.

/// Convert LaTeX-style math into a readable Unicode approximation.
pub fn latex_to_unicode(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let chars: Vec<char> = input.chars().collect();
    let mut i = 0;

    while i < chars.len() {
        let ch = chars[i];

        // LaTeX command: \name or \{name}
        if ch == '\\' && i + 1 < chars.len() {
            let start = i + 1;
            let mut end = start;
            while end < chars.len() && (chars[end].is_ascii_alphabetic()) {
                end += 1;
            }

            if end > start {
                let cmd: String = chars[start..end].iter().collect();

                // Handle `\frac{a}{b}` — parse two brace-enclosed args.
                if cmd == "frac" && end < chars.len() && chars[end] == '{' {
                    let (num, after_num) = parse_braced_arg(&chars, end);
                    if after_num < chars.len() && chars[after_num] == '{' {
                        let (den, after_den) = parse_braced_arg(&chars, after_num);
                        out.push('(');
                        out.push_str(&latex_to_unicode(&num));
                        out.push_str(")/(");
                        out.push_str(&latex_to_unicode(&den));
                        out.push(')');
                        i = after_den;
                        continue;
                    }
                }

                // Handle `\sqrt{x}` — single brace arg.
                if cmd == "sqrt" && end < chars.len() && chars[end] == '{' {
                    let (arg, after) = parse_braced_arg(&chars, end);
                    out.push('√');
                    out.push('(');
                    out.push_str(&latex_to_unicode(&arg));
                    out.push(')');
                    i = after;
                    continue;
                }

                // Handle `\text{...}` — passthrough.
                if cmd == "text" && end < chars.len() && chars[end] == '{' {
                    let (arg, after) = parse_braced_arg(&chars, end);
                    out.push_str(&arg);
                    i = after;
                    continue;
                }

                // Simple command → Unicode lookup.
                if let Some(unicode) = command_to_unicode(&cmd) {
                    out.push_str(unicode);
                    i = end;
                    continue;
                }

                // Unknown command — emit as-is.
                out.push('\\');
                out.push_str(&cmd);
                i = end;
                continue;
            }
        }

        // Superscript: `x^2` or `x^{2n}`.
        if ch == '^' && i + 1 < chars.len() {
            i += 1;
            if chars[i] == '{' {
                let (arg, after) = parse_braced_arg(&chars, i);
                // Recursively process first so \pi → π then superscriptize.
                let processed = latex_to_unicode(&arg);
                for c in processed.chars() {
                    out.push(to_superscript(c));
                }
                i = after;
            } else {
                out.push(to_superscript(chars[i]));
                i += 1;
            }
            continue;
        }

        // Subscript: `a_i` or `a_{ij}`.
        if ch == '_' && i + 1 < chars.len() {
            i += 1;
            if chars[i] == '{' {
                let (arg, after) = parse_braced_arg(&chars, i);
                let processed = latex_to_unicode(&arg);
                for c in processed.chars() {
                    out.push(to_subscript(c));
                }
                i = after;
            } else {
                out.push(to_subscript(chars[i]));
                i += 1;
            }
            continue;
        }

        // Strip braces from grouped expressions.
        if ch == '{' || ch == '}' {
            i += 1;
            continue;
        }

        out.push(ch);
        i += 1;
    }

    out
}

/// Parse a brace-enclosed argument starting at position `pos` (must point at `{`).
/// Returns (inner_content, position_after_closing_brace).
fn parse_braced_arg(chars: &[char], pos: usize) -> (String, usize) {
    if pos >= chars.len() || chars[pos] != '{' {
        return (String::new(), pos);
    }
    let mut depth = 1;
    let mut j = pos + 1;
    let mut inner = String::new();
    while j < chars.len() && depth > 0 {
        if chars[j] == '{' {
            depth += 1;
            inner.push(chars[j]);
        } else if chars[j] == '}' {
            depth -= 1;
            if depth == 0 {
                j += 1;
                break;
            }
            inner.push(chars[j]);
        } else {
            inner.push(chars[j]);
        }
        j += 1;
    }
    (inner, j)
}

/// Map a LaTeX command name to its Unicode equivalent.
fn command_to_unicode(cmd: &str) -> Option<&'static str> {
    Some(match cmd {
        // Greek — lowercase
        "alpha" => "α", "beta" => "β", "gamma" => "γ", "delta" => "δ",
        "epsilon" => "ε", "varepsilon" => "ε",
        "zeta" => "ζ", "eta" => "η", "theta" => "θ", "vartheta" => "ϑ",
        "iota" => "ι", "kappa" => "κ", "lambda" => "λ", "mu" => "μ",
        "nu" => "ν", "xi" => "ξ", "pi" => "π", "varpi" => "ϖ",
        "rho" => "ρ", "varrho" => "ϱ", "sigma" => "σ", "varsigma" => "ς",
        "tau" => "τ", "upsilon" => "υ", "phi" => "φ", "varphi" => "ϕ",
        "chi" => "χ", "psi" => "ψ", "omega" => "ω",
        // Greek — uppercase
        "Alpha" => "Α", "Beta" => "Β", "Gamma" => "Γ", "Delta" => "Δ",
        "Epsilon" => "Ε", "Zeta" => "Ζ", "Eta" => "Η", "Theta" => "Θ",
        "Iota" => "Ι", "Kappa" => "Κ", "Lambda" => "Λ", "Mu" => "Μ",
        "Nu" => "Ν", "Xi" => "Ξ", "Pi" => "Π", "Rho" => "Ρ",
        "Sigma" => "Σ", "Tau" => "Τ", "Upsilon" => "Υ", "Phi" => "Φ",
        "Chi" => "Χ", "Psi" => "Ψ", "Omega" => "Ω",
        // Operators
        "sum" => "∑", "prod" => "∏", "int" => "∫", "oint" => "∮",
        "partial" => "∂", "nabla" => "∇", "infty" => "∞",
        "cdot" => "·", "times" => "×", "div" => "÷", "pm" => "±", "mp" => "∓",
        "ast" => "∗", "star" => "⋆", "circ" => "∘", "bullet" => "•",
        // Relations
        "leq" => "≤", "le" => "≤", "geq" => "≥", "ge" => "≥",
        "neq" => "≠", "ne" => "≠", "approx" => "≈", "sim" => "∼",
        "equiv" => "≡", "cong" => "≅", "propto" => "∝",
        "ll" => "≪", "gg" => "≫", "subset" => "⊂", "supset" => "⊃",
        "subseteq" => "⊆", "supseteq" => "⊇", "in" => "∈", "notin" => "∉",
        "ni" => "∋", "cup" => "∪", "cap" => "∩", "emptyset" => "∅",
        // Arrows
        "to" => "→", "rightarrow" => "→", "leftarrow" => "←",
        "leftrightarrow" => "↔", "Rightarrow" => "⇒", "Leftarrow" => "⇐",
        "Leftrightarrow" => "⇔", "mapsto" => "↦", "uparrow" => "↑",
        "downarrow" => "↓", "updownarrow" => "↕",
        // Logic
        "forall" => "∀", "exists" => "∃", "neg" => "¬", "lnot" => "¬",
        "land" => "∧", "wedge" => "∧", "lor" => "∨", "vee" => "∨",
        "implies" => "⟹", "iff" => "⟺",
        // Misc
        "ldots" => "…", "cdots" => "⋯", "vdots" => "⋮", "ddots" => "⋱",
        "hbar" => "ℏ", "ell" => "ℓ", "Re" => "ℜ", "Im" => "ℑ",
        "aleph" => "ℵ", "prime" => "′", "angle" => "∠", "perp" => "⊥",
        "parallel" => "∥", "degree" => "°",
        // Functions (common)
        "sin" => "sin", "cos" => "cos", "tan" => "tan",
        "log" => "log", "ln" => "ln", "exp" => "exp",
        "min" => "min", "max" => "max", "lim" => "lim",
        _ => return None,
    })
}

fn to_superscript(ch: char) -> char {
    match ch {
        '0' => '⁰', '1' => '¹', '2' => '²', '3' => '³', '4' => '⁴',
        '5' => '⁵', '6' => '⁶', '7' => '⁷', '8' => '⁸', '9' => '⁹',
        '+' => '⁺', '-' => '⁻', '=' => '⁼', '(' => '⁽', ')' => '⁾',
        'n' => 'ⁿ', 'i' => 'ⁱ',
        'a' => 'ᵃ', 'b' => 'ᵇ', 'c' => 'ᶜ', 'd' => 'ᵈ', 'e' => 'ᵉ',
        'f' => 'ᶠ', 'g' => 'ᵍ', 'h' => 'ʰ', 'j' => 'ʲ', 'k' => 'ᵏ',
        'l' => 'ˡ', 'm' => 'ᵐ', 'o' => 'ᵒ', 'p' => 'ᵖ', 'r' => 'ʳ',
        's' => 'ˢ', 't' => 'ᵗ', 'u' => 'ᵘ', 'v' => 'ᵛ', 'w' => 'ʷ',
        'x' => 'ˣ', 'y' => 'ʸ', 'z' => 'ᶻ',
        _ => ch,
    }
}

fn to_subscript(ch: char) -> char {
    match ch {
        '0' => '₀', '1' => '₁', '2' => '₂', '3' => '₃', '4' => '₄',
        '5' => '₅', '6' => '₆', '7' => '₇', '8' => '₈', '9' => '₉',
        '+' => '₊', '-' => '₋', '=' => '₌', '(' => '₍', ')' => '₎',
        'a' => 'ₐ', 'e' => 'ₑ', 'h' => 'ₕ', 'i' => 'ᵢ', 'j' => 'ⱼ',
        'k' => 'ₖ', 'l' => 'ₗ', 'm' => 'ₘ', 'n' => 'ₙ', 'o' => 'ₒ',
        'p' => 'ₚ', 'r' => 'ᵣ', 's' => 'ₛ', 't' => 'ₜ', 'u' => 'ᵤ',
        'v' => 'ᵥ', 'x' => 'ₓ',
        _ => ch,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_greek_letters() {
        assert_eq!(latex_to_unicode("\\alpha"), "α");
        assert_eq!(latex_to_unicode("\\Omega"), "Ω");
        assert_eq!(latex_to_unicode("\\alpha + \\beta"), "α + β");
    }

    #[test]
    fn test_operators() {
        assert_eq!(latex_to_unicode("\\sum_{i=1}^n"), "∑ᵢ₌₁ⁿ");
        assert_eq!(latex_to_unicode("x \\leq y"), "x ≤ y");
        assert_eq!(latex_to_unicode("\\infty"), "∞");
    }

    #[test]
    fn test_fractions() {
        assert_eq!(latex_to_unicode("\\frac{a}{b}"), "(a)/(b)");
        assert_eq!(latex_to_unicode("\\frac{x+1}{2}"), "(x+1)/(2)");
    }

    #[test]
    fn test_sqrt() {
        assert_eq!(latex_to_unicode("\\sqrt{x}"), "√(x)");
        assert_eq!(latex_to_unicode("\\sqrt{x^2 + y^2}"), "√(x² + y²)");
    }

    #[test]
    fn test_superscripts() {
        assert_eq!(latex_to_unicode("x^2"), "x²");
        assert_eq!(latex_to_unicode("x^{2n}"), "x²ⁿ");
        assert_eq!(latex_to_unicode("e^{i\\pi}"), "eⁱπ");
    }

    #[test]
    fn test_subscripts() {
        assert_eq!(latex_to_unicode("a_1"), "a₁");
        assert_eq!(latex_to_unicode("a_{ij}"), "aᵢⱼ");
    }

    #[test]
    fn test_arrows() {
        assert_eq!(latex_to_unicode("A \\to B"), "A → B");
        assert_eq!(latex_to_unicode("x \\mapsto y"), "x ↦ y");
    }

    #[test]
    fn test_logic() {
        assert_eq!(latex_to_unicode("\\forall x \\in S"), "∀ x ∈ S");
        assert_eq!(latex_to_unicode("P \\implies Q"), "P ⟹ Q");
    }

    #[test]
    fn test_passthrough() {
        assert_eq!(latex_to_unicode("x + y = z"), "x + y = z");
    }

    #[test]
    fn test_unknown_command() {
        assert_eq!(latex_to_unicode("\\notacommand"), "\\notacommand");
    }
}
