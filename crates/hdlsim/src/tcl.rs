//! Minimal Tcl interpreter for `hdlsim do` script files (phase 2 of
//! docs/plan.md). Std-only, no panics on bad input (errors are values).
//!
//! Supported: `#` comments, newline/`;` command separators, `set`/`unset`,
//! `puts`/`echo`, `expr` (integer arithmetic, comparisons, `&&`/`||`/`!`),
//! `if`/`elsif`/`else`, `while`, `for`, `incr`, `break`/`continue`, `source`,
//! `quit`/`exit`, `$var` and `[command]` substitution, `"..."` quotes, and
//! `{braced}` words (kept raw, re-evaluated when used as a body/condition).
//! Anything else is delegated to a caller-supplied command callback (the
//! simulator commands of `hdlsim do`: `run`, `restart`, `vcd`, `examine`).

use std::collections::HashMap;

#[derive(Clone, Debug, PartialEq)]
enum Word {
    /// Unquoted or "quoted" word: raw text, `$`/`[`/backslash substitution
    /// happens when the word is expanded.
    Plain(String),
    /// `{...}` word: raw body, no substitution at expand time (the `if`/`for`
    /// handlers substitute when they evaluate it as an expression).
    Brace(String),
}

/// Control flow out of one command.
#[derive(Clone, Debug, PartialEq)]
pub enum Flow {
    Continue,
    Break,
    LoopContinue,
    Quit(i32),
}

pub struct Interp {
    vars: HashMap<String, String>,
    last_result: String,
    /// `puts`/`echo` lines, kept for tests.
    pub log: Vec<String>,
}

/// Callback for non-builtin commands: receives the expanded argument words.
pub type Cmd<'a> = dyn FnMut(&mut Interp, &[String]) -> Result<Flow, String> + 'a;

impl Interp {
    pub fn new() -> Interp {
        Interp {
            vars: HashMap::new(),
            last_result: String::new(),
            log: Vec::new(),
        }
    }

    pub fn get(&self, name: &str) -> Option<&str> {
        self.vars.get(name).map(|s| s.as_str())
    }

    pub fn set(&mut self, name: &str, value: &str) {
        self.vars.insert(name.to_string(), value.to_string());
    }

    pub fn result(&self) -> &str {
        &self.last_result
    }

    /// Evaluate a whole script.
    pub fn eval(&mut self, script: &str, cmd: &mut Cmd) -> Result<Flow, String> {
        let commands = tokenize(script)?;
        self.eval_commands(&commands, cmd)
    }

    fn eval_commands(&mut self, commands: &[Vec<Word>], cmd: &mut Cmd) -> Result<Flow, String> {
        for c in commands {
            match self.eval_command(c, cmd)? {
                Flow::Continue => {}
                other => return Ok(other),
            }
        }
        Ok(Flow::Continue)
    }

    fn eval_command(&mut self, words: &[Word], cmd: &mut Cmd) -> Result<Flow, String> {
        if words.is_empty() {
            return Ok(Flow::Continue);
        }
        let name = self.expand(&words[0])?;
        let rest = &words[1..];
        match name.as_str() {
            "set" => {
                let all = self.expand_slice(rest)?;
                let n = all[0].clone();
                let v = match all.get(1) {
                    Some(v) => v.clone(),
                    None => self
                        .vars
                        .get(&n)
                        .cloned()
                        .ok_or_else(|| format!("set: no such variable '{}'", n))?,
                };
                self.vars.insert(n, v.clone());
                self.last_result = v;
                Ok(Flow::Continue)
            }
            "unset" => {
                let all = self.expand_slice(rest)?;
                let n = all[0].clone();
                self.vars.remove(&n);
                Ok(Flow::Continue)
            }
            "puts" | "echo" => {
                let args = self.expand_slice(rest)?;
                let line = args.join(" ");
                println!("{}", line);
                self.log.push(line);
                self.last_result = String::new();
                Ok(Flow::Continue)
            }
            "expr" => {
                let raw: Vec<String> = rest
                    .iter()
                    .map(|w| match w {
                        Word::Brace(b) => b.clone(),
                        Word::Plain(p) => p.clone(),
                    })
                    .collect();
                let text = self.subst(&raw.join(" "))?;
                let v = eval_expr(&text)?;
                self.last_result = v.to_string();
                Ok(Flow::Continue)
            }
            "if" => self.cmd_if(rest, cmd),
            "while" => {
                let cond = &rest[0];
                let body = &rest[1];
                loop {
                    if !self.truthy(cond)? {
                        break;
                    }
                    match self.eval_body(body, cmd)? {
                        Flow::Continue | Flow::LoopContinue => {}
                        Flow::Break => break,
                        other => return Ok(other),
                    }
                }
                Ok(Flow::Continue)
            }
            "for" => {
                let init = &rest[0];
                let cond = &rest[1];
                let step = &rest[2];
                let body = &rest[3];
                self.eval_body(init, cmd)?;
                loop {
                    if !self.truthy(cond)? {
                        break;
                    }
                    match self.eval_body(body, cmd)? {
                        Flow::Continue => {}
                        Flow::LoopContinue => {}
                        Flow::Break => break,
                        other => return Ok(other),
                    }
                    self.eval_body(step, cmd)?;
                }
                Ok(Flow::Continue)
            }
            "incr" => {
                let all = self.expand_slice(rest)?;
                let n = all[0].clone();
                let d: i64 = match all.get(1) {
                    Some(v) => v.parse().map_err(|_| "incr: bad delta")?,
                    None => 1,
                };
                let cur: i64 = self
                    .vars
                    .get(&n)
                    .and_then(|v| v.parse().ok())
                    .ok_or_else(|| format!("incr: no such variable '{}'", n))?;
                let v = cur + d;
                self.vars.insert(n, v.to_string());
                self.last_result = v.to_string();
                Ok(Flow::Continue)
            }
            "break" => Ok(Flow::Break),
            "continue" => Ok(Flow::LoopContinue),
            "source" => {
                let all = self.expand_slice(rest)?;
                let path = all[0].clone();
                let text = std::fs::read_to_string(&path)
                    .map_err(|e| format!("source: cannot read {}: {}", path, e))?;
                self.eval(&text, cmd)
            }
            "quit" | "exit" => {
                let all = self.expand_slice(rest)?;
                let code = match all.get(0) {
                    Some(v) => v.parse().map_err(|_| "quit: bad exit code")?,
                    None => 0,
                };
                Ok(Flow::Quit(code))
            }
            _ => {
                let mut args = vec![name];
                args.extend(self.expand_slice(rest)?);
                cmd(self, &args)
            }
        }
    }

    fn cmd_if(&mut self, rest: &[Word], cmd: &mut Cmd) -> Result<Flow, String> {
        // if cond body [elsif cond body]* [else body]
        let mut i = 0;
        loop {
            if i + 2 > rest.len() {
                return Err("if: expected {condition} {body}".to_string());
            }
            let cond = &rest[i];
            let body = &rest[i + 1];
            if self.truthy(cond)? {
                return self.eval_body(body, cmd);
            }
            i += 2;
            if i >= rest.len() {
                return Ok(Flow::Continue);
            }
            let kw = self.expand(&rest[i])?;
            if kw == "elsif" {
                i += 1;
                continue;
            }
            if kw == "else" {
                if i + 1 >= rest.len() {
                    return Err("else: missing body".to_string());
                }
                return self.eval_body(&rest[i + 1], cmd);
            }
            return Err(format!("if: unexpected word '{}'", kw));
        }
    }

    /// Evaluate a braced/plain body as a sub-script.
    fn eval_body(&mut self, body: &Word, cmd: &mut Cmd) -> Result<Flow, String> {
        let text = match body {
            Word::Brace(b) => b.clone(),
            Word::Plain(p) => self.subst(p)?,
        };
        self.eval(&text, cmd)
    }

    /// Condition truthiness: try as integer expression, else string true/1.
    fn truthy(&mut self, cond: &Word) -> Result<bool, String> {
        let text = match cond {
            Word::Brace(b) => self.subst(b)?,
            Word::Plain(p) => self.subst(p)?,
        };
        match eval_expr(&text) {
            Ok(v) => Ok(v != 0),
            Err(_) => Ok(matches!(text.as_str(), "true" | "yes" | "1")),
        }
    }

    /// Expand a word: Plain gets `$var`/`[cmd]`/backslash substitution;
    /// Brace stays raw.
    fn expand(&mut self, w: &Word) -> Result<String, String> {
        match w {
            Word::Plain(p) => self.subst(p),
            Word::Brace(b) => Ok(b.clone()),
        }
    }

    /// Expand a slice of words into argument strings. A standalone `[` word
    /// starts a command substitution that may span several words, up to the
    /// matching `]`.
    fn expand_slice(&mut self, words: &[Word]) -> Result<Vec<String>, String> {
        let mut out = Vec::new();
        let mut i = 0;
        while i < words.len() {
            if let Word::Plain(p) = &words[i] {
                if p == "[" {
                    let mut depth = 1;
                    let mut inner: Vec<String> = Vec::new();
                    let mut j = i + 1;
                    while j < words.len() && depth > 0 {
                        match &words[j] {
                            Word::Plain(q) if q == "[" => {
                                depth += 1;
                                inner.push("[".to_string());
                            }
                            Word::Plain(q) if q == "]" => {
                                depth -= 1;
                                if depth > 0 {
                                    inner.push("]".to_string());
                                }
                            }
                            Word::Plain(q) => inner.push(q.clone()),
                            Word::Brace(b) => inner.push(b.clone()),
                        }
                        j += 1;
                    }
                    if depth != 0 {
                        return Err("unterminated [command]".to_string());
                    }
                    let script = self.subst(&inner.join(" "))?;
                    self.eval(&script, &mut |_i, _a| Ok(Flow::Continue))?;
                    let r = self.last_result.clone();
                    out.push(r);
                    i = j;
                    continue;
                }
            }
            out.push(self.expand(&words[i])?);
            i += 1;
        }
        Ok(out)
    }

    /// `$var` / `[command]` / backslash substitution in a plain string.
    fn subst(&mut self, s: &str) -> Result<String, String> {
        let b = s.as_bytes();
        let mut out = String::new();
        let mut i = 0;
        while i < b.len() {
            match b[i] {
                b'\\' if i + 1 < b.len() => {
                    let c = b[i + 1] as char;
                    out.push(match c {
                        'n' => '\n',
                        't' => '\t',
                        'r' => '\r',
                        other => other,
                    });
                    i += 2;
                }
                b'$' => {
                    let start = i + 1;
                    let mut end = start;
                    if end < b.len() && b[end] == b'{' {
                        end += 1;
                        while end < b.len() && b[end] != b'}' {
                            end += 1;
                        }
                        if end >= b.len() {
                            return Err("unterminated ${...}".to_string());
                        }
                        let name = &s[start + 1..end];
                        let v = self
                            .vars
                            .get(name)
                            .cloned()
                            .ok_or_else(|| format!("no such variable '{}'", name))?;
                        out.push_str(&v);
                        i = end + 1;
                    } else {
                        while end < b.len() && (b[end].is_ascii_alphanumeric() || b[end] == b'_') {
                            end += 1;
                        }
                        if end == start {
                            return Err("bad $ substitution".to_string());
                        }
                        let name = &s[start..end];
                        let v = self
                            .vars
                            .get(name)
                            .cloned()
                            .ok_or_else(|| format!("no such variable '{}'", name))?;
                        out.push_str(&v);
                        i = end;
                    }
                }
                b'[' => {
                    // Command substitution: find the matching ']'.
                    let mut depth = 1;
                    let mut j = i + 1;
                    while j < b.len() && depth > 0 {
                        if b[j] == b'[' {
                            depth += 1;
                        } else if b[j] == b']' {
                            depth -= 1;
                        }
                        j += 1;
                    }
                    if depth != 0 {
                        return Err("unterminated [command]".to_string());
                    }
                    let inner = &s[i + 1..j - 1];
                    let inner = self.subst(inner)?;
                    let flow = self.eval(&inner, &mut |_i, _a| Ok(Flow::Continue))?;
                    let _ = flow;
                    out.push_str(&self.last_result.clone());
                    i = j;
                }
                _ => {
                    // Copy one UTF-8 char.
                    let ch = s[i..].chars().next().unwrap();
                    out.push(ch);
                    i += ch.len_utf8();
                }
            }
        }
        Ok(out)
    }
}

/// Tokenize a script into commands of words.
fn tokenize(script: &str) -> Result<Vec<Vec<Word>>, String> {
    let b = script.as_bytes();
    let mut pos = 0usize;
    let mut commands: Vec<Vec<Word>> = Vec::new();
    let mut cur: Vec<Word> = Vec::new();
    while pos < b.len() {
        let c = b[pos];
        if c == b' ' || c == b'\t' || c == b'\r' {
            pos += 1;
            continue;
        }
        if c == b'\n' || c == b';' {
            if !cur.is_empty() {
                commands.push(std::mem::take(&mut cur));
            }
            pos += 1;
            continue;
        }
        if c == b'#' && cur.is_empty() {
            // Comment to end of line.
            while pos < b.len() && b[pos] != b'\n' {
                pos += 1;
            }
            continue;
        }
        if c == b'{' {
            // Braced word: balanced braces, backslash escapes the next char.
            let start = pos + 1;
            let mut depth = 1;
            let mut i = start;
            while i < b.len() && depth > 0 {
                match b[i] {
                    b'\\' => i += 1,
                    b'{' => depth += 1,
                    b'}' => depth -= 1,
                    _ => {}
                }
                i += 1;
            }
            if depth != 0 {
                return Err("unterminated {brace}".to_string());
            }
            cur.push(Word::Brace(script[start..i - 1].to_string()));
            pos = i;
            continue;
        }
        if c == b'"' {
            // Quoted word.
            let start = pos + 1;
            let mut i = start;
            while i < b.len() && b[i] != b'"' {
                if b[i] == b'\\' {
                    i += 1;
                }
                i += 1;
            }
            if i >= b.len() {
                return Err("unterminated \"quote\"".to_string());
            }
            cur.push(Word::Plain(script[start..i].to_string()));
            pos = i + 1;
            continue;
        }
        // Plain word: to next whitespace/separator; `[` and `]` are their
        // own words so `[command]` substitution can span words.
        if c == b'[' || c == b']' {
            cur.push(Word::Plain(script[pos..pos + 1].to_string()));
            pos += 1;
            continue;
        }
        let start = pos;
        while pos < b.len() && !matches!(b[pos], b' ' | b'\t' | b'\r' | b'\n' | b';' | b'[' | b']')
        {
            pos += 1;
        }
        cur.push(Word::Plain(script[start..pos].to_string()));
    }
    if !cur.is_empty() {
        commands.push(cur);
    }
    Ok(commands)
}

/// Integer expression evaluator (after `$` substitution): precedence
/// `||` < `&&` < `==`/`!=` < `<`/`<=`/`>`/`>=` < `+`/`-` < `*`/`/`/`%` < unary.
pub fn eval_expr(text: &str) -> Result<i64, String> {
    let mut p = ExprParser {
        b: text.as_bytes(),
        pos: 0,
    };
    let v = p.or()?;
    p.ws();
    if p.pos != p.b.len() {
        return Err(format!("expr: trailing '{}'", &text[p.pos..]));
    }
    Ok(v)
}

struct ExprParser<'a> {
    b: &'a [u8],
    pos: usize,
}

impl<'a> ExprParser<'a> {
    fn ws(&mut self) {
        while self.pos < self.b.len() && matches!(self.b[self.pos], b' ' | b'\t') {
            self.pos += 1;
        }
    }
    fn peek(&self) -> Option<u8> {
        self.b.get(self.pos).copied()
    }
    fn eat2(&mut self, s: &[u8]) -> bool {
        if self.b[self.pos..].starts_with(s) {
            self.pos += s.len();
            true
        } else {
            false
        }
    }
    fn or(&mut self) -> Result<i64, String> {
        let mut v = self.and()?;
        loop {
            self.ws();
            if self.eat2(b"||") {
                let r = self.and()?;
                v = ((v != 0) || (r != 0)) as i64;
            } else {
                return Ok(v);
            }
        }
    }
    fn and(&mut self) -> Result<i64, String> {
        let mut v = self.eq()?;
        loop {
            self.ws();
            if self.eat2(b"&&") {
                let r = self.eq()?;
                v = ((v != 0) && (r != 0)) as i64;
            } else {
                return Ok(v);
            }
        }
    }
    fn eq(&mut self) -> Result<i64, String> {
        let mut v = self.rel()?;
        loop {
            self.ws();
            if self.eat2(b"==") {
                let r = self.rel()?;
                v = (v == r) as i64;
            } else if self.eat2(b"!=") {
                let r = self.rel()?;
                v = (v != r) as i64;
            } else {
                return Ok(v);
            }
        }
    }
    fn rel(&mut self) -> Result<i64, String> {
        let mut v = self.add()?;
        loop {
            self.ws();
            if self.eat2(b"<=") {
                let r = self.add()?;
                v = (v <= r) as i64;
            } else if self.eat2(b">=") {
                let r = self.add()?;
                v = (v >= r) as i64;
            } else if self.peek() == Some(b'<') {
                self.pos += 1;
                let r = self.add()?;
                v = (v < r) as i64;
            } else if self.peek() == Some(b'>') {
                self.pos += 1;
                let r = self.add()?;
                v = (v > r) as i64;
            } else {
                return Ok(v);
            }
        }
    }
    fn add(&mut self) -> Result<i64, String> {
        let mut v = self.mul()?;
        loop {
            self.ws();
            match self.peek() {
                Some(b'+') => {
                    self.pos += 1;
                    v += self.mul()?;
                }
                Some(b'-') => {
                    self.pos += 1;
                    v -= self.mul()?;
                }
                _ => return Ok(v),
            }
        }
    }
    fn mul(&mut self) -> Result<i64, String> {
        let mut v = self.unary()?;
        loop {
            self.ws();
            match self.peek() {
                Some(b'*') => {
                    self.pos += 1;
                    v = v.checked_mul(self.unary()?).ok_or("expr: overflow")?;
                }
                Some(b'/') => {
                    self.pos += 1;
                    let r = self.unary()?;
                    if r == 0 {
                        return Err("expr: division by zero".to_string());
                    }
                    v = v.checked_div(r).ok_or("expr: overflow")?;
                }
                Some(b'%') => {
                    self.pos += 1;
                    let r = self.unary()?;
                    if r == 0 {
                        return Err("expr: modulo by zero".to_string());
                    }
                    v = v.checked_rem(r).ok_or("expr: overflow")?;
                }
                _ => return Ok(v),
            }
        }
    }
    fn unary(&mut self) -> Result<i64, String> {
        self.ws();
        match self.peek() {
            Some(b'!') => {
                self.pos += 1;
                Ok((self.unary()? == 0) as i64)
            }
            Some(b'-') => {
                self.pos += 1;
                Ok(-self.unary()?)
            }
            Some(b'+') => {
                self.pos += 1;
                self.unary()
            }
            _ => self.primary(),
        }
    }
    fn primary(&mut self) -> Result<i64, String> {
        self.ws();
        match self.peek() {
            Some(b'(') => {
                self.pos += 1;
                let v = self.or()?;
                self.ws();
                if self.peek() == Some(b')') {
                    self.pos += 1;
                    Ok(v)
                } else {
                    Err("expr: missing )".to_string())
                }
            }
            Some(c) if c.is_ascii_digit() => {
                let start = self.pos;
                if self.b[self.pos..].starts_with(b"0x") || self.b[self.pos..].starts_with(b"0X") {
                    self.pos += 2;
                    while self.pos < self.b.len() && self.b[self.pos].is_ascii_hexdigit() {
                        self.pos += 1;
                    }
                    i64::from_str_radix(&String::from_utf8_lossy(&self.b[start + 2..self.pos]), 16)
                        .map_err(|_| "expr: bad hex".to_string())
                } else {
                    while self.pos < self.b.len() && self.b[self.pos].is_ascii_digit() {
                        self.pos += 1;
                    }
                    String::from_utf8_lossy(&self.b[start..self.pos])
                        .parse()
                        .map_err(|_| "expr: bad number".to_string())
                }
            }
            _ => Err(format!("expr: unexpected '{}'", self.pos)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(script: &str) -> (Interp, Flow) {
        let mut it = Interp::new();
        let flow = it
            .eval(script, &mut |_i, args| {
                Ok(Flow::Continue).map(|f| {
                    let _ = args;
                    f
                })
            })
            .unwrap();
        (it, flow)
    }

    #[test]
    fn set_subst_and_expr() {
        let (it, _) = run("set a 5\nset b [expr $a * 2 + 1]\nexpr $b - 1");
        assert_eq!(it.result(), "10");
        assert_eq!(it.get("b"), Some("11"));
    }

    #[test]
    fn expr_precedence_and_logic() {
        assert_eq!(eval_expr("1 + 2 * 3").unwrap(), 7);
        assert_eq!(eval_expr("(1 + 2) * 3").unwrap(), 9);
        assert_eq!(eval_expr("1 < 2 && 3 >= 3").unwrap(), 1);
        assert_eq!(eval_expr("!(1 == 2) || 0").unwrap(), 1);
        assert_eq!(eval_expr("0x10 + 2").unwrap(), 18);
        assert_eq!(eval_expr("7 % 3").unwrap(), 1);
        assert!(eval_expr("1 / 0").is_err());
    }

    #[test]
    fn if_while_for_break_continue() {
        let (it, _) = run(
            "set s 0\n\
             if {$s == 0} { set s 1 } else { set s 99 }\n\
             while {$s < 5} { incr s }\n\
             for {set i 0} {$i < 10} {incr i} { if {$i == 3} { continue }; if {$i > 6} { break }; incr s }\n\
             expr $s",
        );
        // s: 1 -> while makes 5; for: i=0..6, skip i==3, break at i>6 -> s += 6
        assert_eq!(it.result(), "11");
    }

    #[test]
    fn comments_quotes_braces_semicolons() {
        let (it, _) = run("# a comment\nset x \"a b\" ; puts $x\nexpr 1 ; # trailing\n");
        assert_eq!(it.get("x"), Some("a b"));
        assert_eq!(it.log, vec!["a b".to_string()]);
    }

    #[test]
    fn quit_and_unknown_command() {
        let (_, flow) = run("quit 3");
        assert_eq!(flow, Flow::Quit(3));
        let mut it = Interp::new();
        let r = it.eval("frobnicate 1", &mut |_i, args| {
            assert_eq!(args, &["frobnicate".to_string(), "1".to_string()]);
            Ok(Flow::Continue)
        });
        assert!(r.is_ok());
        assert!(Interp::new()
            .eval("set $x", &mut |_, _| Ok(Flow::Continue))
            .is_err());
        assert!(Interp::new()
            .eval("expr 1 +", &mut |_, _| Ok(Flow::Continue))
            .is_err());
    }
}
