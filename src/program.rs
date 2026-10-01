// FOCAL program engine for xrpn: parse .xrpn text, run it over a CalcState.
// Mirrors XRPN's program semantics — labels, GTO/XEQ/GSB/RTN/END, the
// conditional skip-next family, ISG/DSE counters (ISG fixed to increment),
// and VIEW/AVIEW/PROMPT/PSE output. Non-flow lines delegate to the calculator
// `execute`. Pure: the caller (Kotlin) holds the program + run cursor and calls
// run_program / step; PROMPT/STOP return control so the UI can resume.

use super::engine::{
    alpha_append, canon, commit_entry, execute, lift_stack, norm_flag, recall_reg, CalcState,
};

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
/// comment lines (starting with '#' or ';') are dropped; trailing inline
/// comments are stripped. Everything else is kept verbatim (labels included).
///
/// An HP-41 listing loads as it is: when every line carries a step number
/// (`001*LBL "X"`, `02 X<>Y`), the numbers and the `*` label marks go, and
/// `.import` style directive lines are dropped.
#[cfg_attr(feature = "uniffi", uniffi::export)]
pub fn parse_program(name: String, text: String) -> Program {
    let mut lines: Vec<String> = Vec::new();
    for raw in text.lines() {
        let s = raw.trim();
        let directive = s.starts_with('.') && s.chars().nth(1).is_some_and(|c| c.is_alphabetic());
        if s.is_empty() || s.starts_with('#') || s.starts_with(';') || directive {
            continue;
        }
        lines.push(s.to_string());
    }
    let numbered = !lines.is_empty() && lines.iter().all(|l| step_body(l).is_some());
    let lines = lines
        .iter()
        .map(|l| if numbered { step_body(l).unwrap_or(l) } else { l.as_str() })
        .map(strip_comment)
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .collect();
    Program { name, lines }
}

/// The instruction after a listing's step number, or None when the line has
/// no step number. `022 *` is a multiply; `001*LBL "X"` is a marked label.
fn step_body(line: &str) -> Option<&str> {
    let digits = line.chars().take_while(|c| c.is_ascii_digit()).count();
    let rest = &line[digits..];
    if digits < 2 || !rest.starts_with(|c: char| c.is_whitespace() || c == '*' || c == '\u{25b8}') {
        return None;
    }
    let body = rest.trim_start();
    let unmarked = body
        .strip_prefix('*')
        .or_else(|| body.strip_prefix('\u{25b8}'))
        .map(str::trim_start)
        .filter(|b| !b.is_empty());
    Some(unmarked.unwrap_or(body)).filter(|b| !b.is_empty())
}

/// Cut a trailing `# …` or `; …` comment, but never inside a quoted Alpha
/// string, and never the `#` of `X#0?` (a comment mark follows a space).
fn strip_comment(line: &str) -> &str {
    if line.contains('"') {
        return line;
    }
    let cut = line
        .char_indices()
        .find(|&(i, c)| c == ';' || (c == '#' && line[..i].ends_with(char::is_whitespace)))
        .map_or(line.len(), |(i, _)| i);
    line[..cut].trim()
}

enum Instr {
    Number(f64),
    Alpha { text: String, append: bool },
    Cmd { name: String, arg: Option<String> },
}

fn classify(line: &str) -> Instr {
    let t = line.trim();
    // Number literal: optional sign, digits, comma/dot, exponent.
    if let Some(n) = parse_number(t) {
        return Instr::Number(n);
    }
    // Alpha string:  "text"  or an append:  >"text"  "|text"  "|-text"
    if let Some(rest) = alpha_append(t) {
        return Instr::Alpha { text: rest.trim_end_matches('"').to_string(), append: true };
    }
    if t.starts_with('"') {
        let inner = t.trim_matches('"').to_string();
        return Instr::Alpha { text: inner, append: false };
    }
    // Command (name + optional arg). Lowercase the name; keep arg verbatim.
    let mut parts = t.splitn(2, char::is_whitespace);
    let name = canon(&parts.next().unwrap_or("").to_lowercase()).to_string();
    let arg = parts.next().map(|a| a.trim().to_string()).filter(|a| !a.is_empty());
    Instr::Cmd { name, arg }
}

/// A number line: `5`, `-2,5`, `1e3`, and the HP-41 listing forms `1 E3`,
/// `E2` (a bare exponent means mantissa 1) and `E` (which is 1).
fn parse_number(t: &str) -> Option<f64> {
    let s = t.replace(" E", "e").replace(" e", "e").to_ascii_lowercase();
    if s.is_empty() || !s.chars().all(|c| c.is_ascii_digit() || matches!(c, '.' | ',' | '-' | '+' | 'e')) {
        return None;
    }
    let s = s.replacen(',', ".", 1);
    let (mant, exp) = match s.split_once('e') {
        Some((m, e)) => (m, Some(e)),
        None => (s.as_str(), None),
    };
    if exp.is_none() && !mant.contains(|c: char| c.is_ascii_digit()) {
        return None; // "+", "-" and "." are commands or nothing
    }
    let mant = match mant {
        "" | "+" => "1",
        "-" => "-1",
        m => m,
    };
    match exp {
        Some(e) if e.contains(|c: char| c.is_ascii_digit()) => format!("{mant}e{e}").parse().ok(),
        _ => mant.parse().ok(),
    }
}

/// Find the line index of a label. Accepts:
///   gto/xeq "NAME"  -> matches `lbl "NAME"`
///   gto/xeq NN      -> matches `lbl NN`
///   gto .N          -> absolute line number N (1-based)
/// A quoted label is looked for from the top. A numeric or one-letter label
/// is looked for from the line after `from` and on around, as an HP-41
/// does, so a program may use the same number twice.
fn find_label(prog: &Program, target: &str, from: usize) -> Option<usize> {
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
    let n = prog.lines.len();
    let start = if target.starts_with('"') { 0 } else { from + 1 };
    (0..n).map(|k| (start + k) % n).find(|&i| {
        let lt = prog.lines[i].trim();
        lt.get(..4).is_some_and(|p| p.eq_ignore_ascii_case("lbl "))
            && normalize_label_arg(&lt[4..]) == want
    })
}

/// A label as it is compared: no quotes, and `01` the same as `1`.
fn normalize_label_arg(a: &str) -> String {
    let a = a.trim().trim_matches('"');
    if !a.is_empty() && a.chars().all(|c| c.is_ascii_digit()) {
        let n = a.trim_start_matches('0');
        return if n.is_empty() { "0".to_string() } else { n.to_string() };
    }
    a.to_string()
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
    // A number still being keyed is finished first, as R/S does on an HP-41.
    commit_entry(&mut calc);
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
                        match arg.as_deref().and_then(|a| find_label(&program, a, pc)) {
                            Some(t) => next_pc = t,
                            None => {
                                return done(calc, pc, rstack, output, RunStatus::Error,
                                    Some(format!("No such label: {}", arg.unwrap_or_default())));
                            }
                        }
                    }
                    "xeq" | "gsb" => {
                        match arg.as_deref().and_then(|a| find_label(&program, a, pc)) {
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
                        // VIEW nn shows a register; a bare VIEW shows X.
                        let v = arg.as_deref().map_or(calc.x, |r| recall_reg(&calc, r));
                        output.push(super::engine::format_value(&calc, v));
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
                    // FS?C and FC?C test a flag, then clear it.
                    "fs?c" | "fc?c" => {
                        let set = flag_set(&calc, arg.as_deref());
                        if let Some(f) = arg.as_deref() {
                            calc.flags.insert(norm_flag(f), false);
                        }
                        if set != (name == "fs?c") {
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
        Some(f) => *c.flags.get(&norm_flag(f)).unwrap_or(&false), // "01" is flag 1
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

    // ---- HP-41 listings, loaded as they are --------------------------------

    /// Run from a quoted label with `x` in X (and `y` in Y).
    fn run_label(p: &Program, label: &str, y: f64, x: f64, alpha: &str) -> RunResult {
        let pc = find_label(p, &format!("\"{label}\""), 0).expect("label");
        let mut c = new_state();
        c.y = y;
        c.x = x;
        c.alpha = alpha.to_string();
        run_program(c, p.clone(), pc as u32, vec![], false, 0)
    }

    const SUBN: &str = "001 *LBL \"SUBN\"\n002  32\n003  X<>Y\n004  -\n005  2\n006  X<>Y\n007  Y^X\n008  END\n";

    #[test]
    fn a_numbered_listing_loses_its_step_numbers() {
        let p = prog(SUBN);
        assert_eq!(p.lines, vec!["LBL \"SUBN\"", "32", "X<>Y", "-", "2", "X<>Y", "Y^X", "END"]);
        // A /26 subnet holds 2^(32-26) addresses.
        assert_eq!(run_label(&p, "SUBN", 0.0, 26.0, "").calc.x, 64.0);
    }

    #[test]
    fn a_plain_program_keeps_lines_that_start_with_digits() {
        let p = prog("10\nenter\n24 E5\n+\n# note\nsto 01 ; kept\nend");
        assert_eq!(p.lines, vec!["10", "enter", "24 E5", "+", "sto 01", "end"]);
        assert_eq!(run_all(&p).calc.x, 2_400_010.0);
    }

    #[test]
    fn listing_number_forms() {
        for (text, want) in [("E2", 100.0), ("1 E3", 1000.0), ("E", 1.0), ("2,5 E-1", 0.25), (".4", 0.4)] {
            assert_eq!(parse_number(text), Some(want), "{text}");
        }
        for text in ["-", "+", ".", "E^X", "1/X"] {
            assert_eq!(parse_number(text), None, "{text}");
        }
    }

    #[test]
    fn a_multiply_step_and_stack_registers() {
        // COMB and PERM from the GEIR ROM: `022 *` is a multiply, not a mark.
        let p = prog(
            "001 LBL \"COMB\"\n002 LBL 01 \n003 ENTER\n004 FACT\n005 1/X\n006 STO T\n007 RDN\n008 XEQ 02\n\
             009 *\n010 RTN\n011 GTO 01 \n012 LBL \"PERM\"\n013 LBL 02 \n014 RCL Y\n015 FACT\n016 RDN\n017 -\n\
             018 FACT\n019 1/X\n020 R^\n022 *\n023 RTN\n024 GTO 02 \n025 END\n",
        );
        assert_eq!(run_label(&p, "COMB", 5.0, 2.0, "").calc.x, 10.0);
        assert_eq!(run_label(&p, "PERM", 5.0, 2.0, "").calc.x, 20.0);
    }

    #[test]
    fn hash_in_a_test_is_no_comment() {
        // Lambert W, with `X#0?` closing the loop: W(1) = 0.5671...
        let p = prog(
            "001*LBL \"LW\"\n002 STO M\n003 0\n004 STO N\n005*LBL 00\n006 RCL N\n007 ENTER\n008 ENTER\n009 E^X\n\
             010 *\n011 LASTX\n012 1\n013 R^\n014 +\n015 *\n016 LASTX\n017 1/X\n018 1\n019 +\n020 2\n021 /\n\
             022 R^\n023 RCL M\n024 -\n025 ENTER\n026 RDN\n027 *\n028 -\n029 /\n030 ST- N\n031 ABS\n032 RND\n\
             033 X#0?\n034 GTO 00\n035 RCL N\n036 RTN\n",
        );
        let r = run_label(&p, "LW", 0.0, 1.0, "");
        assert_eq!(r.status, RunStatus::Ended);
        assert!((r.calc.x - 0.567143).abs() < 1e-4, "W(1) = {}", r.calc.x);
    }

    const LUHN: &str = "001 LBL \"LUHN\"\n002 0\n003 STO 00\n004 CF 01\n005 SF 02\n006 LBL 00\n007 ATOX\n008 X=0?\n\
        009 GTO 01\n010 48\n011 -\n012 ENTER\n013 FS? 02\n014 +\n015 FS?C 02\n016 SF 05\n017 FS?C 01\n018 SF 02\n\
        019 FS?C 05\n020 SF 01\n021 9\n022 X<>Y\n023 X>Y?\n024 XEQ 02\n025 ST+ 00\n026 GTO 00\n027 LBL 02\n028 10\n\
        029 /\n030 ENTER\n031 INT\n032 X<>Y\n033 FRC\n034 10\n035 *\n036 +\n037 RTN\n038 LBL 01\n039 CF 01\n\
        040 CF 02\n041 RCL 00\n042 10\n043 /\n044 FRC\n045 10\n046 *\n047 \"INVALID\"\n048 X=0?\n049 \"VALID\"\n\
        050 AVIEW \n051 END \n";

    #[test]
    fn flags_with_a_leading_zero_and_an_empty_alpha() {
        // LUHN reads Alpha a character at a time until ATOX gives 0, and
        // flips flags 01, 02 and 05 with FS?C.
        let p = prog(LUHN);
        let said = |digits: &str| run_label(&p, "LUHN", 0.0, 0.0, digits).output.last().cloned();
        assert_eq!(said("1234567812345670").as_deref(), Some("VALID"));
        assert_eq!(said("1234567812345678").as_deref(), Some("INVALID"));
    }

    #[test]
    fn an_alpha_append_as_a_listing_prints_it() {
        // CLUMIN: the brightness of a hex colour held in Alpha.
        let p = prog(
            "001*LBL \"CLUMIN\"\n002 GTO 00\n003 \"FMT=FFFFFF\"\n004*LBL 00 \n005 3\n006 STO 01\n007 0\n008 STO 00\n\
             009*LBL 01 \n010 XEQ 10\n011 DSE 01\n012 GTO 01\n013 RCL 00\n014 765\n015 /\n016 100\n017 *\n\
             018 \"LUM=\"\n019 ARCL X\n020 \"|-%\"\n021 PROMPT\n022 GTO 00\n023*LBL 10\n024 XEQ 11\n025 16\n026 *\n\
             027 ST+ 00\n028 XEQ 11\n029 ST+ 00\n030 RTN \n031*LBL 11\n032 ATOX\n033 48\n034 -\n035 16\n036 X>Y?\n\
             037 GTO 12\n038 -\n039 9\n040 +\n041 0\n042*LBL 12\n043 RDN\n044 END\n",
        );
        let r = run_label(&p, "CLUMIN", 0.0, 0.0, "FFFFFF");
        assert_eq!(r.status, RunStatus::Prompt);
        assert_eq!(r.output.last().map(String::as_str), Some("LUM=100,0000%"));
    }

    #[test]
    fn a_numeric_label_is_found_forward_first() {
        // Two LBL 00: each GTO 00 takes the next one down, as on an HP-41.
        let p = prog("gto 00\nlbl 00\n1\nstop\ngto 00\nlbl 00\n2\nstop");
        assert_eq!(run_all(&p).calc.x, 1.0);
        assert_eq!(run_program(new_state(), p.clone(), 4, vec![], false, 0).calc.x, 2.0);
    }

    #[test]
    fn a_number_being_keyed_is_finished_before_the_run() {
        let mut c = new_state();
        c = super::super::engine::key_digit(c, "2".into());
        c = super::super::engine::key_digit(c, "6".into());
        let r = run_program(c, prog(SUBN), 0, vec![], false, 0);
        assert_eq!(r.calc.x, 64.0);
    }

    #[test]
    fn register_commands_reach_the_stack() {
        // ST+ Y adds X to Y. X<> 01 swaps X with a register. VIEW 01 shows it.
        let p = prog("5\nenter\n3\nst+ y\nx<> 01\nview 01\nend");
        let r = run_all(&p);
        assert_eq!((r.calc.y, r.calc.x), (8.0, 0.0));
        assert!(r.output[0].starts_with('3'), "{:?}", r.output);
    }

    #[test]
    fn runaway_guard() {
        // gto self with no exit -> StepCap, not a hang.
        let p = prog("lbl \"X\"\ngto \"X\"");
        let r = run_program(new_state(), p, 0, vec![], false, 500);
        assert_eq!(r.status, RunStatus::StepCap);
    }
}
