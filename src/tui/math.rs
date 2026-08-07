//! LaTeX math → Unicode text conversion (ported from rsmarkdown-tui).
//!
//! Renders the common math subset as Unicode glyphs (∫ ∑ √ Greek letters,
//! sub/superscripts), the approach used by terminal markdown viewers like
//! innomd. Unknown commands fall back to their plain name; anything exotic
//! degrades to readable ASCII rather than failing.

/// Convert LaTeX math to Unicode text for terminal display.
pub fn latex_to_unicode(src: &str) -> String {
    // stray `$` are pipeline artifacts (single-dollar rewrite), not content
    let clean: String = src.chars().filter(|c| *c != '$').collect();
    let mut p = Parser {
        chars: clean.chars().collect(),
        pos: 0,
    };
    let out = p.parse_expr();
    out.trim().to_string()
}

struct Parser {
    chars: Vec<char>,
    pos: usize,
}

impl Parser {
    fn peek(&self) -> Option<char> {
        self.chars.get(self.pos).copied()
    }

    fn next(&mut self) -> Option<char> {
        let c = self.peek();
        if c.is_some() {
            self.pos += 1;
        }
        c
    }

    /// Parse until `}` or end; collapses runs of spaces.
    fn parse_expr(&mut self) -> String {
        let mut out = String::new();
        while let Some(c) = self.peek() {
            match c {
                '^' => {
                    self.next();
                    out.push_str(&self.parse_script(true));
                }
                '_' => {
                    self.next();
                    out.push_str(&self.parse_script(false));
                }
                '{' => {
                    self.next();
                    out.push_str(&self.parse_group());
                }
                '}' => {
                    self.next();
                    break;
                }
                '\\' => {
                    self.next();
                    out.push_str(&self.parse_command());
                }
                ' ' => {
                    self.next();
                    if !out.ends_with(' ') {
                        out.push(' ');
                    }
                }
                '$' => {
                    self.next();
                }
                c => {
                    self.next();
                    out.push(if c == '-' { '\u{2212}' } else { c });
                }
            }
        }
        out
    }

    /// `{...}` contents (opening brace already consumed).
    fn parse_group(&mut self) -> String {
        self.parse_expr()
    }

    /// One argument: `{...}` group, a command, or a single character.
    fn parse_arg(&mut self) -> String {
        match self.peek() {
            Some('{') => {
                self.next();
                self.parse_group()
            }
            Some('\\') => {
                self.next();
                self.parse_command()
            }
            Some(c) => {
                self.next();
                c.to_string()
            }
            None => String::new(),
        }
    }

    /// `^`/`_` argument -> Unicode script run, falling back to `^(...)` / `_(...)`.
    fn parse_script(&mut self, superscript: bool) -> String {
        let arg = self.parse_arg();
        let map = if superscript {
            SUPERSCRIPTS
        } else {
            SUBSCRIPTS
        };
        if let Some(run) = script_run(&arg, map) {
            return run;
        }
        if superscript {
            format!("^({})", arg)
        } else {
            format!("_({})", arg)
        }
    }

    fn parse_command(&mut self) -> String {
        // `\` consumed; read the command name (letters) or a symbol char
        let name: String = {
            let mut n = String::new();
            while let Some(c) = self.peek() {
                if c.is_ascii_alphabetic() {
                    n.push(c);
                    self.next();
                } else {
                    break;
                }
            }
            n
        };
        let symbol = if name.is_empty() {
            self.next().map(|c| c.to_string()).unwrap_or_default()
        } else {
            String::new()
        };

        match name.as_str() {
            "frac" => {
                let a = self.parse_arg();
                let b = self.parse_arg();
                format!("{}⁄{}", parens_frac(&a), parens_frac(&b))
            }
            "binom" => {
                let a = self.parse_arg();
                let b = self.parse_arg();
                format!("({} {})", a, b)
            }
            "sqrt" => {
                // optional [n] index
                if self.peek() == Some('[') {
                    self.next();
                    let mut n = String::new();
                    while let Some(c) = self.peek() {
                        if c == ']' {
                            self.next();
                            break;
                        }
                        n.push(c);
                        self.next();
                    }
                    let arg = self.parse_arg();
                    let root = match n.as_str() {
                        "3" => "\u{221b}",
                        "4" => "\u{221c}",
                        _ => return format!("{}√({})", n, arg),
                    };
                    if arg.chars().count() == 1 {
                        format!("{}{}", root, arg)
                    } else {
                        format!("{}({})", root, arg)
                    }
                } else {
                    let arg = self.parse_arg();
                    if arg.chars().count() == 1 {
                        format!("√{}", arg)
                    } else {
                        format!("√({})", arg)
                    }
                }
            }
            "text" | "mathrm" | "mathit" | "mathbf" | "mathsf" | "operatorname" => self.parse_arg(),
            "mathbb" => blackboard(&self.parse_arg()),
            "left" | "right" | "big" | "Big" | "bigg" | "Bigg" | "bigl" | "bigr" | "Bigl"
            | "Bigr" | "biggl" | "biggr" | "Biggl" | "Biggr" | "limits" | "nolimits"
            | "displaystyle" | "textstyle" | "scriptstyle" | "scriptsize" | "notag" => {
                String::new()
            }
            "quad" | "qquad" | ";" | ":" => "  ".to_string(),
            "," | " " => " ".to_string(),
            "!" => String::new(),
            "hat" | "bar" | "vec" | "tilde" | "dot" | "ddot" | "overline" | "underbrace" => {
                let arg = self.parse_arg();
                let combining = match name.as_str() {
                    "hat" => "\u{0302}",
                    "bar" | "overline" => "\u{0304}",
                    "vec" => "\u{20d7}",
                    "tilde" => "\u{0303}",
                    "dot" => "\u{0307}",
                    "ddot" => "\u{0308}",
                    _ => "",
                };
                if arg.chars().count() == 1 {
                    format!("{}{}", arg, combining)
                } else {
                    // apply to the last char of the group
                    let mut s = arg;
                    let last = s.chars().last().map(|c| c.to_string()).unwrap_or_default();
                    s.truncate(s.len() - last.len());
                    format!("{}{}{}", s, last, combining)
                }
            }
            "begin" => self.parse_environment(),
            _ => {
                if !name.is_empty() {
                    // named command: symbol table lookup, else operator name as text
                    if let Some((_, sym)) = SYMBOLS.iter().find(|(k, _)| *k == name.as_str()) {
                        return (*sym).to_string();
                    }
                    name
                } else {
                    // symbol command like `\%`, `\{`, `\\`
                    match symbol.as_str() {
                        "{" => "{".to_string(),
                        "}" => "}".to_string(),
                        "%" | "&" | "#" | "_" | "^" | "$" => symbol,
                        "\\" => " ".to_string(),
                        "," | ":" | ";" | " " => " ".to_string(),
                        "" => String::new(),
                        _ => symbol,
                    }
                }
            }
        }
    }

    /// `\begin{env} ... \end{env}` — matrices and friends.
    fn parse_environment(&mut self) -> String {
        let env = self.parse_arg();
        let mut rows: Vec<Vec<String>> = Vec::new();
        let mut cells: Vec<String> = Vec::new();
        let mut cell = String::new();
        let mut done = false;
        while !done {
            match self.peek() {
                None => done = true,
                Some('&') => {
                    self.next();
                    cells.push(cell.trim().to_string());
                    cell.clear();
                }
                Some('\\') => {
                    // lookahead for `\end{...}`
                    let save = self.pos;
                    self.next();
                    if self.peek() == Some('e') {
                        let mut name = String::new();
                        while let Some(c) = self.peek() {
                            if c.is_ascii_alphabetic() {
                                name.push(c);
                                self.next();
                            } else {
                                break;
                            }
                        }
                        if name == "end" {
                            let _ = self.parse_arg();
                            cells.push(cell.trim().to_string());
                            rows.push(cells.clone());
                            done = true;
                            continue;
                        }
                    }
                    // row break `\\`; optional `[spacing]` arg is dropped
                    self.pos = save;
                    self.next();
                    if self.peek() == Some('\\') {
                        self.next();
                    }
                    if self.peek() == Some('[') {
                        while let Some(c) = self.next() {
                            if c == ']' {
                                break;
                            }
                        }
                    }
                    cells.push(cell.trim().to_string());
                    cell.clear();
                    rows.push(std::mem::take(&mut cells));
                }
                Some(c) => {
                    self.next();
                    cell.push(c);
                }
            }
        }
        let inner: Vec<String> = rows
            .iter()
            .map(|r| {
                r.iter()
                    .filter(|c| !c.is_empty())
                    .cloned()
                    .collect::<Vec<_>>()
                    .join("  ")
            })
            .collect();
        match env.as_str() {
            "pmatrix" => format!("({})", inner.join(" ; ")),
            "bmatrix" => format!("[{}]", inner.join(" ; ")),
            "Bmatrix" => format!("{{{}}}", inner.join(" ; ")),
            "vmatrix" => format!("|{}|", inner.join(" ; ")),
            "Vmatrix" => format!("‖{}‖", inner.join(" ; ")),
            "cases" => format!("{{ {} }}", inner.join(" ; ")),
            _ => inner.join(" ; "),
        }
    }
}

fn parens_frac(arg: &str) -> String {
    if arg.chars().count() == 1 {
        arg.to_string()
    } else if has_top_level_operator(arg) {
        format!("({})", arg)
    } else {
        arg.to_string()
    }
}

/// True when `+ - = < >` appears outside any `( ... )` nesting.
fn has_top_level_operator(arg: &str) -> bool {
    let mut depth = 0usize;
    for c in arg.chars() {
        match c {
            '(' => depth += 1,
            ')' => depth = depth.saturating_sub(1),
            '+' | '-' | '=' | '<' | '>' if depth == 0 => return true,
            _ => {}
        }
    }
    false
}

fn blackboard(arg: &str) -> String {
    arg.chars()
        .map(|c| match c {
            'R' => "ℝ".to_string(),
            'Z' => "ℤ".to_string(),
            'Q' => "ℚ".to_string(),
            'N' => "ℕ".to_string(),
            'C' => "ℂ".to_string(),
            'P' => "ℙ".to_string(),
            'H' => "ℍ".to_string(),
            'E' => "𝔼".to_string(),
            _ => c.to_string(),
        })
        .collect()
}

/// Convert a run to script glyphs; None if any char has no mapping.
fn script_run(arg: &str, map: &[(&str, &str)]) -> Option<String> {
    let mut out = String::new();
    for c in arg.chars() {
        let key: &str = Box::leak(c.to_string().into_boxed_str());
        let sym = map.iter().find(|(k, _)| *k == key)?;
        out.push_str(sym.1);
    }
    Some(out)
}

const SUPERSCRIPTS: &[(&str, &str)] = &[
    ("0", "⁰"),
    ("1", "¹"),
    ("2", "²"),
    ("3", "³"),
    ("4", "⁴"),
    ("5", "⁵"),
    ("6", "⁶"),
    ("7", "⁷"),
    ("8", "⁸"),
    ("9", "⁹"),
    ("+", "⁺"),
    ("-", "⁻"),
    ("=", "⁼"),
    ("(", "⁽"),
    (")", "⁾"),
    ("a", "ᵃ"),
    ("b", "ᵇ"),
    ("c", "ᶜ"),
    ("d", "ᵈ"),
    ("e", "ᵉ"),
    ("f", "ᶠ"),
    ("g", "ᵍ"),
    ("h", "ʰ"),
    ("i", "ⁱ"),
    ("j", "ʲ"),
    ("k", "ᵏ"),
    ("l", "ˡ"),
    ("m", "ᵐ"),
    ("n", "ⁿ"),
    ("o", "ᵒ"),
    ("p", "ᵖ"),
    ("r", "ʳ"),
    ("s", "ˢ"),
    ("t", "ᵗ"),
    ("u", "ᵘ"),
    ("v", "ᵛ"),
    ("w", "ʷ"),
    ("x", "ˣ"),
    ("y", "ʸ"),
    ("z", "ᶻ"),
];

const SUBSCRIPTS: &[(&str, &str)] = &[
    ("0", "₀"),
    ("1", "₁"),
    ("2", "₂"),
    ("3", "₃"),
    ("4", "₄"),
    ("5", "₅"),
    ("6", "₆"),
    ("7", "₇"),
    ("8", "₈"),
    ("9", "₉"),
    ("+", "₊"),
    ("-", "₋"),
    ("=", "₌"),
    ("(", "₍"),
    (")", "₎"),
    ("a", "ₐ"),
    ("e", "ₑ"),
    ("h", "ₕ"),
    ("i", "ᵢ"),
    ("j", "ⱼ"),
    ("k", "ₖ"),
    ("l", "ₗ"),
    ("m", "ₘ"),
    ("n", "ₙ"),
    ("o", "ₒ"),
    ("p", "ₚ"),
    ("r", "ᵣ"),
    ("s", "ₛ"),
    ("t", "ₜ"),
    ("u", "ᵤ"),
    ("v", "ᵥ"),
    ("x", "ₓ"),
];

const SYMBOLS: &[(&str, &str)] = &[
    // greek lowercase
    ("alpha", "α"),
    ("beta", "β"),
    ("gamma", "γ"),
    ("delta", "δ"),
    ("epsilon", "ε"),
    ("zeta", "ζ"),
    ("eta", "η"),
    ("theta", "θ"),
    ("iota", "ι"),
    ("kappa", "κ"),
    ("lambda", "λ"),
    ("mu", "μ"),
    ("nu", "ν"),
    ("xi", "ξ"),
    ("omicron", "ο"),
    ("pi", "π"),
    ("rho", "ρ"),
    ("sigma", "σ"),
    ("tau", "τ"),
    ("upsilon", "υ"),
    ("phi", "φ"),
    ("chi", "χ"),
    ("psi", "ψ"),
    ("omega", "ω"),
    ("varepsilon", "ε"),
    ("vartheta", "ϑ"),
    ("varphi", "φ"),
    ("varrho", "ϱ"),
    ("varpi", "ϖ"),
    ("varsigma", "ς"),
    // greek uppercase
    ("Gamma", "Γ"),
    ("Delta", "Δ"),
    ("Theta", "Θ"),
    ("Lambda", "Λ"),
    ("Xi", "Ξ"),
    ("Pi", "Π"),
    ("Sigma", "Σ"),
    ("Upsilon", "Υ"),
    ("Phi", "Φ"),
    ("Psi", "Ψ"),
    ("Omega", "Ω"),
    // operators
    ("int", "∫"),
    ("iint", "∬"),
    ("iiint", "∭"),
    ("oint", "∮"),
    ("sum", "Σ"),
    ("prod", "∏"),
    ("coprod", "∐"),
    ("times", "×"),
    ("div", "÷"),
    ("pm", "±"),
    ("mp", "∓"),
    ("cdot", "·"),
    ("cdots", "⋯"),
    ("ldots", "…"),
    ("dots", "…"),
    ("vdots", "⋮"),
    ("ddots", "⋱"),
    ("neq", "≠"),
    ("ne", "≠"),
    ("le", "≤"),
    ("leq", "≤"),
    ("ge", "≥"),
    ("geq", "≥"),
    ("ll", "≪"),
    ("gg", "≫"),
    ("approx", "≈"),
    ("cong", "≅"),
    ("equiv", "≡"),
    ("sim", "∼"),
    ("simeq", "≃"),
    ("propto", "∝"),
    ("in", "∈"),
    ("notin", "∉"),
    ("ni", "∋"),
    ("notni", "∌"),
    ("subset", "⊂"),
    ("supset", "⊃"),
    ("subseteq", "⊆"),
    ("supseteq", "⊇"),
    ("subsetneq", "⊊"),
    ("supsetneq", "⊋"),
    ("cup", "∪"),
    ("cap", "∩"),
    ("setminus", "∖"),
    ("emptyset", "∅"),
    ("varnothing", "∅"),
    ("partial", "∂"),
    ("nabla", "∇"),
    ("infty", "∞"),
    ("hbar", "ℏ"),
    ("ell", "ℓ"),
    ("aleph", "ℵ"),
    ("imath", "ı"),
    ("jmath", "ȷ"),
    ("prime", "′"),
    ("doubleprime", "″"),
    ("dagger", "†"),
    ("ddagger", "‡"),
    ("degree", "°"),
    ("angle", "∠"),
    ("triangle", "△"),
    ("forall", "∀"),
    ("exists", "∃"),
    ("nexists", "∄"),
    ("neg", "¬"),
    ("lnot", "¬"),
    ("land", "∧"),
    ("wedge", "∧"),
    ("lor", "∨"),
    ("vee", "∨"),
    ("to", "→"),
    ("rightarrow", "→"),
    ("gets", "←"),
    ("leftarrow", "←"),
    ("Leftarrow", "⇐"),
    ("Rightarrow", "⇒"),
    ("leftrightarrow", "↔"),
    ("Leftrightarrow", "⇔"),
    ("mapsto", "↦"),
    ("implies", "⇒"),
    ("therefore", "∴"),
    ("because", "∵"),
    ("perp", "⊥"),
    ("parallel", "∥"),
    ("mid", "∣"),
    ("nmid", "∤"),
    ("oplus", "⊕"),
    ("ominus", "⊖"),
    ("otimes", "⊗"),
    ("oslash", "⊘"),
    ("odot", "⊙"),
    ("star", "⋆"),
    ("circ", "∘"),
    ("bullet", "∙"),
    ("bigcirc", "○"),
    ("square", "□"),
    ("diamond", "◇"),
    ("lozenge", "◆"),
    ("clubsuit", "♣"),
    ("diamondsuit", "♢"),
    ("heartsuit", "♡"),
    ("spadesuit", "♠"),
    ("checkmark", "✓"),
    ("Re", "ℜ"),
    ("Im", "ℑ"),
    ("wp", "℘"),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn integral_with_limits() {
        assert_eq!(
            latex_to_unicode(r"\int_a^b f(x)\,dx = F(b) - F(a)"),
            "∫ₐᵇ f(x) dx = F(b) − F(a)"
        );
    }

    #[test]
    fn greek_and_symbols() {
        assert_eq!(latex_to_unicode(r"\lambda = \frac{b}{T}"), "λ = b⁄T");
        assert_eq!(
            latex_to_unicode(r"\alpha \beta \gamma \leq \infty"),
            "α β γ ≤ ∞"
        );
    }

    #[test]
    fn fraction() {
        assert_eq!(latex_to_unicode(r"\frac{1}{2}"), "1⁄2");
        assert_eq!(latex_to_unicode(r"\frac{a+b}{c}"), "(a+b)⁄c");
        assert_eq!(latex_to_unicode(r"\frac{n(n+1)}{2}"), "n(n+1)⁄2");
    }

    #[test]
    fn sqrt_and_binom() {
        assert_eq!(latex_to_unicode(r"\sqrt{x}"), "√x");
        assert_eq!(latex_to_unicode(r"\sqrt{2x+1}"), "√(2x+1)");
        assert_eq!(latex_to_unicode(r"\sqrt[3]{x}"), "∛x");
        assert_eq!(latex_to_unicode(r"\binom{n}{k}"), "(n k)");
    }

    #[test]
    fn sum_with_scripts() {
        assert_eq!(
            latex_to_unicode(r"\sum_{i=1}^n x_i = \frac{n(n+1)}{2}"),
            "Σᵢ₌₁ⁿ xᵢ = n(n+1)⁄2"
        );
    }

    #[test]
    fn text_and_mathbb() {
        assert_eq!(latex_to_unicode(r"\mathbb{R}^n"), "ℝⁿ");
        assert_eq!(latex_to_unicode(r"\text{if } x > 0"), "if x > 0");
    }

    #[test]
    fn matrix() {
        assert_eq!(
            latex_to_unicode(r"\begin{pmatrix} a & b \\ c & d \end{pmatrix}"),
            "(a  b ; c  d)"
        );
    }

    #[test]
    fn unknown_command_falls_back() {
        assert_eq!(latex_to_unicode(r"\foobar{x}"), "foobarx");
        assert_eq!(latex_to_unicode(r"x \dagger y"), "x † y");
    }

    #[test]
    fn stray_dollars_stripped() {
        assert_eq!(latex_to_unicode(r"x^2$ math"), "x² math");
    }

    #[test]
    fn accents() {
        assert_eq!(latex_to_unicode(r"\hat{x}"), "x\u{0302}");
        assert_eq!(latex_to_unicode(r"\vec{F}"), "F\u{20d7}");
    }
}
