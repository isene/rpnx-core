// FOCAL program engine for xrpn: parse .xrpn text, run it over a CalcState.
// Mirrors XRPN's program semantics — labels, GTO/XEQ/GSB/RTN/END, the
// conditional skip-next family, ISG/DSE counters (ISG fixed to increment),
// and VIEW/AVIEW/PROMPT/PSE output. Non-flow lines delegate to the calculator
// `execute`. Pure: the caller (Kotlin) holds the program + run cursor and calls
// run_program / step; PROMPT/STOP return control so the UI can resume.

use super::engine::{execute, lift_stack, CalcState};

#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct Program {
    pub name: String,
    pub lines: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
pub enum RunStatus {
    Ended,   // fell off the end / END / RTN with empty stack
    Stopped, // STOP / R/S — resume by running again from pc
    Prompt,  // PROMPT — awaiting input, then resume from pc
    Error,   // a command errored
    StepCap, // hit the step limit (runaway guard)
}

#[derive(Debug, Clone)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct RunResult {
    pub calc: CalcState,
    pub pc: u32,
    pub return_stack: Vec<u32>,
    pub output: Vec<String>,
    pub status: RunStatus,
    pub message: Option<String>,
}

/// Parse program text into cleaned instruction lines. Blank lines and full
/// comment lines (starting with '#') are dropped; trailing inline `# …`
/// comments are stripped. Everything else is kept verbatim (labels included).
#[cfg_attr(feature = "uniffi", uniffi::export)]
pub fn parse_program(name: String, text: String) -> Program {
    let mut lines = Vec::new();
    for raw in text.lines() {
        let mut s = raw.trim().to_string();
        if s.is_empty() || s.starts_with('#') {
            continue;
        }
        // Strip an inline comment, but not inside a quoted alpha string.
        if !s.contains('"') {
            if let Some(idx) = s.find('#') {
                s = s[..idx].trim().to_string();
            }
        }
        if !s.is_empty() {
            lines.push(s);
        }
    }
    Program { name, lines }
}

enum Instr {
    Number(f64),
    Alpha { text: String, append: bool },
    Cmd { name: String, arg: Option<String> },
}

fn classify(line: &str) -> Instr {
    let t = line.trim();
    // Number literal: optional sign, digits, comma/dot, exponent.
    if is_number(t) {
        let norm = t.replacen(',', ".", 1);
        return Instr::Number(norm.parse::<f64>().unwrap_or(0.0));
    }
    // Alpha string:  "text"  or  >"text" (append)  or  "|text" (append)
    if let Some(rest) = t.strip_prefix(">\"") {
        return Instr::Alpha { text: rest.trim_end_matches('"').to_string(), append: true };
    }
    if let Some(rest) = t.strip_prefix("\"|") {
        return Instr::Alpha { text: rest.trim_end_matches('"').to_string(), append: true };
    }
    if t.starts_with('"') {
        let inner = t.trim_matches('"').to_string();
        return Instr::Alpha { text: inner, append: false };
    }
    // Command (name + optional arg). Lowercase the name; keep arg verbatim.
    let mut parts = t.splitn(2, char::is_whitespace);
    let name = parts.next().unwrap_or("").to_lowercase();
    let arg = parts.next().map(|a| a.trim().to_string()).filter(|a| !a.is_empty());
    Instr::Cmd { name, arg }
}

fn is_number(s: &str) -> bool {
    if s.is_empty() {
        return false;
    }
    let mut seen_digit = false;
    for (i, c) in s.chars().enumerate() {
        match c {
            '0'..='9' => seen_digit = true,
            '.' | ',' => {}
            '-' | '+' => {}
            'e' | 'E' => {}
            _ => return false,
        }
        // a leading letter that's not part of a number is rejected above.
        let _ = i;
    }
    seen_digit
}

/// Find the line index of a label. Accepts:
///   gto/xeq "NAME"  -> matches `lbl "NAME"`
///   gto/xeq NN      -> matches `lbl NN`
///   gto .N          -> absolute line number N (1-based)
fn find_label(prog: &Program, target: &str) -> Option<usize> {
    let target = target.trim();
    if let Some(num) = target.strip_prefix('.') {
        if let Ok(n) = num.parse::<usize>() {
            if n >= 1 && n <= prog.lines.len() {
                return Some(n - 1);
            }
        }
        return None;
    }
    let want = normalize_label_arg(target);
    for (i, line) in prog.lines.iter().enumerate() {
        let lt = line.trim();
        if let Some(rest) = lt.strip_prefix("lbl ").or_else(|| lt.strip_prefix("LBL ")) {
            if normalize_label_arg(rest.trim()) == want {
                return Some(i);
            }
        }
    }
    None
}

fn normalize_label_arg(a: &str) -> String {
    a.trim().trim_matches('"').to_string()
}

#[cfg_attr(feature = "uniffi", uniffi::export)]
pub fn run_program(
    calc: CalcState,
    program: Program,
    pc: u32,
    return_stack: Vec<u32>,
    single_step: bool,
    max_steps: u32,
) -> RunResult {
    let mut calc = calc;
    let mut pc = pc as usize;
    let mut rstack = return_stack;
    let mut output: Vec<String> = Vec::new();
    let mut steps: u32 = 0;
    let cap = if max_steps == 0 { 100_000 } else { max_steps };

    loop {
        if pc >= program.lines.len() {
            return done(calc, pc, rstack, output, RunStatus::Ended, None);
        }
        let line = program.lines[pc].clone();
        let instr = classify(&line);
        let mut next_pc = pc + 1;

        match instr {
            Instr::Number(n) => {
                if calc.lift_enabled {
                    lift_stack(&mut calc);
                }
                calc.x = n;
                calc.lift_enabled = true;
            }
            Instr::Alpha { text, append } => {
                if append {
                    calc.alpha.push_str(&text);
                } else {
                    calc.alpha = text;
                }
            }
            Instr::Cmd { name, arg } => {
                match name.as_str() {
                    "lbl" => {} // marker
                    "gto" => {
                        match arg.as_deref().and_then(|a| find_label(&program, a)) {
                            Some(t) => next_pc = t,
                            None => {
                                return done(calc, pc, rstack, output, RunStatus::Error,
                                    Some(format!("No such label: {}", arg.unwrap_or_default())));
                            }
                        }
                    }
                    "xeq" | "gsb" => {
                        match arg.as_deref().and_then(|a| find_label(&program, a)) {
                            Some(t) => {
                                rstack.push((pc + 1) as u32);
                                next_pc = t;
                            }
                            None => {
                                return done(calc, pc, rstack, output, RunStatus::Error,
                                    Some(format!("No such label: {}", arg.unwrap_or_default())));
                            }
                        }
                    }
                    "rtn" | "end" => {
                        match rstack.pop() {
                            Some(ret) => next_pc = ret as usize,
                            None => {
                                return done(calc, pc + 1, rstack, output, RunStatus::Ended, None);
                            }
                        }
                    }
                    "stop" | "r/s" | "rs" => {
                        return done(calc, pc + 1, rstack, output, RunStatus::Stopped, None);
                    }
                    "view" => {
                        output.push(super::engine::format_value(&calc, calc.x));
                    }
                    "aview" => {
                        output.push(calc.alpha.clone());
                    }
                    "pse" => {
                        output.push(super::engine::format_value(&calc, calc.x));
                    }
                    "prompt" => {
                        output.push(calc.alpha.clone());
                        return done(calc, pc + 1, rstack, output, RunStatus::Prompt, None);
                    }
                    "isg" | "dse" => {
                        let skip = isg_dse(&mut calc, &name, arg.as_deref());
                        if skip {
                            next_pc = pc + 2;
                        }
                    }
                    _ if is_conditional(&name) => {
                        if !eval_conditional(&calc, &name, arg.as_deref()) {
                            next_pc = pc + 2; // skip next on false
                        }
                    }
                    _ => {
                        // Delegate to the calculator command set.
                        let r = execute(calc.clone(), full_cmd(&name, &arg));
                        calc = r.state;
                        if let Some(msg) = r.error {
                            return done(calc, pc, rstack, output, RunStatus::Error, Some(msg));
                        }
                    }
                }
            }
        }

        pc = next_pc;
        steps += 1;
        if single_step {
            let status = if pc >= program.lines.len() { RunStatus::Ended } else { RunStatus::Stopped };
            return done(calc, pc, rstack, output, status, None);
        }
        if steps >= cap {
            return done(calc, pc, rstack, output, RunStatus::StepCap, None);
        }
    }
}

fn done(
    calc: CalcState,
    pc: usize,
    return_stack: Vec<u32>,
    output: Vec<String>,
    status: RunStatus,
    message: Option<String>,
) -> RunResult {
    RunResult { calc, pc: pc as u32, return_stack, output, status, message }
}

fn full_cmd(name: &str, arg: &Option<String>) -> String {
    match arg {
        Some(a) => format!("{} {}", name, a),
        None => name.to_string(),
    }
}

fn is_conditional(name: &str) -> bool {
    matches!(
        name,
        "xeq0" | "xneq0" | "xlt0" | "xgt0" | "xlteq0" | "xgteq0"
            | "xeqy" | "xneqy" | "xlty" | "xgty" | "xlteqy" | "xgteqy"
            | "fs" | "fc" | "fs?" | "fc?"
    )
}

fn eval_conditional(c: &CalcState, name: &str, arg: Option<&str>) -> bool {
    match name {
        "xeq0" => c.x == 0.0,
        "xneq0" => c.x != 0.0,
        "xlt0" => c.x < 0.0,
        "xgt0" => c.x > 0.0,
        "xlteq0" => c.x <= 0.0,
        "xgteq0" => c.x >= 0.0,
        "xeqy" => c.x == c.y,
        "xneqy" => c.x != c.y,
        "xlty" => c.x < c.y,
        "xgty" => c.x > c.y,
        "xlteqy" => c.x <= c.y,
        "xgteqy" => c.x >= c.y,
        "fs" | "fs?" => flag_set(c, arg),
        "fc" | "fc?" => !flag_set(c, arg),
        _ => true,
    }
}

fn flag_set(c: &CalcState, arg: Option<&str>) -> bool {
    match arg {
        Some(f) => *c.flags.get(f.trim()).unwrap_or(&false),
        None => false,
    }
}

/// ISG/DSE on the control number in a register or stack reg. Returns true if
/// the next program line should be skipped. Control number format ccc.fffii:
/// ccc = counter, fff = end, ii = increment (default 1).
fn isg_dse(c: &mut CalcState, which: &str, arg: Option<&str>) -> bool {
    let key = arg.unwrap_or("x").trim().to_string();
    let cur = read_ctl(c, &key);
    let (mut b, e, i) = x2bei(cur);
    let i2 = if i == 0 { 1 } else { i };
    let skip;
    if which == "isg" {
        b += i2; // increment (XRPN desktop had this as decrement — fixed)
        skip = b > e;
    } else {
        b -= i2; // dse: decrement
        skip = b <= e;
    }
    let nv = bei2x(b, e, i);
    write_ctl(c, &key, nv);
    skip
}

fn read_ctl(c: &CalcState, key: &str) -> f64 {
    match key.to_lowercase().as_str() {
        "x" => c.x,
        "y" => c.y,
        "z" => c.z,
        "t" => c.t,
        "l" => c.l,
        _ => *c.reg.get(&reg_key(key)).unwrap_or(&0.0),
    }
}
fn write_ctl(c: &mut CalcState, key: &str, v: f64) {
    match key.to_lowercase().as_str() {
        "x" => c.x = v,
        "y" => c.y = v,
        "z" => c.z = v,
        "t" => c.t = v,
        "l" => c.l = v,
        _ => { c.reg.insert(reg_key(key), v); }
    }
}
fn reg_key(r: &str) -> String {
    let trimmed = r.trim().trim_start_matches('0');
    if trimmed.is_empty() { "0".to_string() } else { trimmed.to_string() }
}

// Control-number decode (ccc.fffii). Same intent as XRPN's xlib/bei, but
// robust to binary-float dust: scale by 100000 and round once, then split the
// integer, so a control like 1.003 decodes to (b=1, e=3, i=1) rather than
// (1, 2, …) as the naive float-trunc would give.
fn x2bei(x: f64) -> (i64, i64, i64) {
    let b = x.trunc() as i64;
    let total = (x.abs() * 100000.0).round() as i64;
    let rem = total - b.abs() * 100000; // fffii as a <=5-digit integer
    let e = rem / 100;
    let i = rem % 100;
    let i = if i == 0 { 1 } else { i };
    (b, e, i)
}
fn bei2x(b: i64, e: i64, i: i64) -> f64 {
    let i = if i == 0 { 1 } else { i };
    b as f64 + (e as f64 / 1000.0) + (i as f64 / 100000.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::engine::new_state;

    fn prog(text: &str) -> Program {
        parse_program("t".into(), text.into())
    }
    fn run_all(p: &Program) -> RunResult {
        run_program(new_state(), p.clone(), 0, vec![], false, 0)
    }

    #[test]
    fn parse_strips_comments_and_blanks() {
        let p = prog("# header\n\n  5\nsto 00  # store\n\"hi\"\n");
        assert_eq!(p.lines, vec!["5", "sto 00", "\"hi\""]);
    }

    #[test]
    fn linear_arithmetic() {
        // 5 ENTER 3 + -> 8
        let p = prog("5\nenter\n3\n+\nend");
        let r = run_all(&p);
        assert_eq!(r.status, RunStatus::Ended);
        assert_eq!(r.calc.x, 8.0);
    }

    #[test]
    fn gto_loop_with_dse() {
        // Count down from 3, summing into reg 1. DSE on a control 3.000.
        // reg1 = 3+2+1 = 6.
        let p = prog(
            "0\nsto 01\n3\nsto 00\nlbl \"L\"\nrcl 00\nint\nstplus 01\ndse 00\ngto \"L\"\nrcl 01\nend",
        );
        let r = run_all(&p);
        assert_eq!(r.status, RunStatus::Ended);
        assert_eq!(r.calc.x, 6.0);
    }

    #[test]
    fn isg_counts_up() {
        // ISG control 1.003 => counter steps 1->2->3->(4 skip). Sum ints = 6.
        let p = prog(
            "0\nsto 01\n1,003\nsto 00\nlbl \"L\"\nrcl 00\nint\nstplus 01\nisg 00\ngto \"L\"\nrcl 01\nend",
        );
        let r = run_all(&p);
        assert_eq!(r.status, RunStatus::Ended);
        assert_eq!(r.calc.x, 6.0);
    }

    #[test]
    fn conditional_skip() {
        // x=0? skips the next line when false. Here X=5 (not 0) -> skip the
        // "100" line, so X stays 5.
        let p = prog("5\nxeq0\n100\nend");
        let r = run_all(&p);
        assert_eq!(r.calc.x, 5.0);
    }

    #[test]
    fn conditional_true_does_not_skip() {
        // X=0 -> xeq0 true -> do NOT skip -> 100 executes (lifts) -> X=100.
        let p = prog("0\nxeq0\n100\nend");
        let r = run_all(&p);
        assert_eq!(r.calc.x, 100.0);
    }

    #[test]
    fn xeq_subroutine_returns() {
        // main: 10, XEQ "DBL", then +1 -> 21. DBL doubles X.
        let p = prog(
            "10\nxeq \"DBL\"\n1\n+\nstop\nlbl \"DBL\"\n2\n*\nrtn",
        );
        let r = run_all(&p);
        // stop halts after the +; X should be 21.
        assert_eq!(r.calc.x, 21.0);
        assert_eq!(r.status, RunStatus::Stopped);
    }

    #[test]
    fn prompt_halts_and_resumes() {
        let p = prog("\"ENTER N\"\nprompt\n2\n*\nend");
        let r = run_all(&p);
        assert_eq!(r.status, RunStatus::Prompt);
        assert_eq!(r.output, vec!["ENTER N".to_string()]);
        // Resume: user keyed 21 and pressed R/S — entry terminated, lift armed.
        let mut c = r.calc;
        c.x = 21.0;
        c.lift_enabled = true;
        let r2 = run_program(c, p.clone(), r.pc, r.return_stack, false, 0);
        assert_eq!(r2.status, RunStatus::Ended);
        assert_eq!(r2.calc.x, 42.0);
    }

    #[test]
    fn view_collects_output() {
        let p = prog("7\nview\nend");
        let r = run_all(&p);
        assert_eq!(r.output.len(), 1);
        assert!(r.output[0].starts_with('7'));
    }

    #[test]
    fn single_step_advances_one() {
        let p = prog("5\nenter\n3\n+\nend");
        let r1 = run_program(new_state(), p.clone(), 0, vec![], true, 0);
        assert_eq!(r1.pc, 1); // executed the "5"
        assert_eq!(r1.calc.x, 5.0);
        assert_eq!(r1.status, RunStatus::Stopped);
    }

    #[test]
    fn runaway_guard() {
        // gto self with no exit -> StepCap, not a hang.
        let p = prog("lbl \"X\"\ngto \"X\"");
        let r = run_program(new_state(), p, 0, vec![], false, 500);
        assert_eq!(r.status, RunStatus::StepCap);
    }
}
