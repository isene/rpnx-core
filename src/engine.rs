// The calculator engine: state model + entry state machine + command set.
// Pure functions over CalcState (a UniFFI Record). Mirrors the desktop XRPN
// stack semantics: binary ops set Last X, compute Y∘X, then drop the stack;
// ENTER lifts and disables the next stack lift (so the next digit overwrites X).

use super::format::to_num;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Full calculator state. Small enough to cross the FFI per keypress.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct CalcState {
    pub x: f64,
    pub y: f64,
    pub z: f64,
    pub t: f64,
    pub l: f64, // Last X
    pub alpha: String,
    /// Numbered registers, keyed by index as a string ("0".."n"). Sparse.
    pub reg: HashMap<String, f64>,
    /// Alpha (string) register store: where ASTO parks the Alpha register and
    /// ARCL recalls it. Separate from `reg` because the numeric stack/registers
    /// can't hold text. Keyed like `reg`. Sparse; defaulted for old states.
    #[serde(default)]
    pub areg: HashMap<String, String>,
    /// Statistics summation registers live at srg.. (XRPN default 11).
    pub stat_base: i32,
    pub flags: HashMap<String, bool>,
    pub decimals: i32,  // FIX n  (i)
    pub threshold: i32, // exponent threshold (s): FIX 9, SCI/ENG 6
    pub grouping: i32,  // exponent grouping (g): ENG 3 else 1
    pub angle_mode: String, // "deg" | "rad" | "grad"
    /// In-progress number entry buffer ("" = not entering). Uses '.' internally.
    pub entering: String,
    /// Stack lift armed: true => the next fresh number pushes X up first.
    pub lift_enabled: bool,
}

/// Result of executing a command: the new state plus an optional error message
/// (the command still returns a usable state on error — X is left as-was).
#[derive(Debug, Clone)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct StepResult {
    pub state: CalcState,
    pub error: Option<String>,
}

/// Formatted snapshot for the UI.
#[derive(Debug, Clone)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct CalcDisplay {
    pub x: String,
    pub y: String,
    pub z: String,
    pub t: String,
    pub last_x: String,
    pub alpha: String,
    pub mode: String, // e.g. "FIX 4  DEG"
    pub entering: bool,
}

#[cfg_attr(feature = "uniffi", uniffi::export)]
pub fn new_state() -> CalcState {
    let mut flags = HashMap::new();
    flags.insert("28".to_string(), false); // dot? false => comma (XRPN default)
    flags.insert("29".to_string(), true); // thousands separator on
    CalcState {
        x: 0.0,
        y: 0.0,
        z: 0.0,
        t: 0.0,
        l: 0.0,
        alpha: String::new(),
        reg: HashMap::new(),
        areg: HashMap::new(),
        stat_base: 11,
        flags,
        decimals: 4,
        threshold: 9,
        grouping: 1,
        angle_mode: "deg".to_string(),
        entering: String::new(),
        lift_enabled: true,
    }
}

/// Serialize the full state for persistence across launches.
#[cfg_attr(feature = "uniffi", uniffi::export)]
pub fn serialize_state(state: CalcState) -> String {
    serde_json::to_string(&state).unwrap_or_default()
}

/// Restore a persisted state; falls back to a fresh state on any error.
#[cfg_attr(feature = "uniffi", uniffi::export)]
pub fn parse_state(json: String) -> CalcState {
    serde_json::from_str(&json).unwrap_or_else(|_| new_state())
}

fn comma(s: &CalcState) -> bool {
    !*s.flags.get("28").unwrap_or(&false)
}
fn sep(s: &CalcState) -> bool {
    *s.flags.get("29").unwrap_or(&true)
}

fn fmt(s: &CalcState, v: f64) -> String {
    to_num(v, s.decimals, s.threshold, s.grouping, comma(s), sep(s))
}

/// Crate-internal helpers reused by the program engine (not FFI-exported).
pub fn format_value(s: &CalcState, v: f64) -> String {
    fmt(s, v)
}
pub fn lift_stack(s: &mut CalcState) {
    lift(s);
}

#[cfg_attr(feature = "uniffi", uniffi::export)]
pub fn display(state: CalcState) -> CalcDisplay {
    let entering = !state.entering.is_empty();
    let x_disp = if entering {
        // Show the raw entry buffer (swap dot for comma if in comma mode).
        if comma(&state) {
            state.entering.replacen('.', ",", 1)
        } else {
            state.entering.clone()
        }
    } else {
        fmt(&state, state.x)
    };
    let mode_word = match state.angle_mode.as_str() {
        "rad" => "RAD",
        "grad" => "GRAD",
        _ => "DEG",
    };
    let fix_word = if state.threshold == 6 && state.grouping == 3 {
        format!("ENG {}", state.decimals)
    } else if state.threshold == 6 {
        format!("SCI {}", state.decimals)
    } else {
        format!("FIX {}", state.decimals)
    };
    CalcDisplay {
        x: x_disp,
        y: fmt(&state, state.y),
        z: fmt(&state, state.z),
        t: fmt(&state, state.t),
        last_x: fmt(&state, state.l),
        alpha: state.alpha.clone(),
        mode: format!("{}  {}", fix_word, mode_word),
        entering,
    }
}

// -------------------------------------------------------------- number entry

fn begin_entry_if_needed(s: &mut CalcState) {
    if s.entering.is_empty() {
        // Starting a fresh number: lift the stack if armed (suppressed right
        // after ENTER/CLx, which leave lift disabled so the digit overwrites X).
        if s.lift_enabled {
            s.t = s.z;
            s.z = s.y;
            s.y = s.x;
        }
        s.x = 0.0;
        s.entering = String::new();
        // The nolift is consumed by this number; re-arm for the next event.
        s.lift_enabled = true;
    }
}

pub(crate) fn commit_entry(s: &mut CalcState) {
    if s.entering.is_empty() {
        return;
    }
    s.x = parse_entry(&s.entering);
    s.entering = String::new();
}

fn parse_entry(buf: &str) -> f64 {
    if buf.is_empty() || buf == "-" || buf == "." || buf == "-." {
        return 0.0;
    }
    buf.parse::<f64>().unwrap_or(0.0)
}

#[cfg_attr(feature = "uniffi", uniffi::export)]
pub fn key_digit(mut state: CalcState, d: String) -> CalcState {
    let ch = d.chars().next().unwrap_or('0');
    if !ch.is_ascii_digit() {
        return state;
    }
    begin_entry_if_needed(&mut state);
    state.entering.push(ch);
    state.x = parse_entry(&state.entering);
    state
}

#[cfg_attr(feature = "uniffi", uniffi::export)]
pub fn key_dot(mut state: CalcState) -> CalcState {
    begin_entry_if_needed(&mut state);
    if !state.entering.contains('.') && !state.entering.contains('e') {
        if state.entering.is_empty() || state.entering == "-" {
            state.entering.push('0');
        }
        state.entering.push('.');
    }
    state.x = parse_entry(&state.entering);
    state
}

#[cfg_attr(feature = "uniffi", uniffi::export)]
pub fn key_eex(mut state: CalcState) -> CalcState {
    begin_entry_if_needed(&mut state);
    if !state.entering.contains('e') {
        if state.entering.is_empty() || state.entering == "-" {
            state.entering.push('1');
        }
        state.entering.push('e');
    }
    state.x = parse_entry(&state.entering);
    state
}

/// CHS: negate the entry buffer (mantissa or exponent) while entering, else
/// negate X directly.
#[cfg_attr(feature = "uniffi", uniffi::export)]
pub fn key_chs(mut state: CalcState) -> CalcState {
    if state.entering.is_empty() {
        state.x = -state.x;
        return state;
    }
    if let Some(epos) = state.entering.find('e') {
        // Toggle exponent sign.
        let (head, tail) = state.entering.split_at(epos + 1);
        let new_tail = if let Some(rest) = tail.strip_prefix('-') {
            rest.to_string()
        } else {
            format!("-{}", tail)
        };
        state.entering = format!("{}{}", head, new_tail);
    } else if let Some(rest) = state.entering.strip_prefix('-') {
        state.entering = rest.to_string();
    } else {
        state.entering = format!("-{}", state.entering);
    }
    state.x = parse_entry(&state.entering);
    state
}

#[cfg_attr(feature = "uniffi", uniffi::export)]
pub fn key_backspace(mut state: CalcState) -> CalcState {
    if state.entering.is_empty() {
        // Not entering: clear X (CLx behaviour).
        state.x = 0.0;
        state.lift_enabled = false;
        return state;
    }
    state.entering.pop();
    if state.entering.is_empty() || state.entering == "-" {
        state.entering = String::new();
        state.x = 0.0;
        state.lift_enabled = false;
    } else {
        state.x = parse_entry(&state.entering);
    }
    state
}

#[cfg_attr(feature = "uniffi", uniffi::export)]
pub fn key_enter(mut state: CalcState) -> CalcState {
    commit_entry(&mut state);
    // lift, then disable next lift (so the next digit overwrites X).
    state.t = state.z;
    state.z = state.y;
    state.y = state.x;
    state.lift_enabled = false;
    state
}

#[cfg_attr(feature = "uniffi", uniffi::export)]
pub fn key_clx(mut state: CalcState) -> CalcState {
    state.entering = String::new();
    state.x = 0.0;
    state.lift_enabled = false;
    state
}

// ----------------------------------------------------------------- commands

fn dropy(s: &mut CalcState) {
    s.y = s.z;
    s.z = s.t;
}
fn drop_full(s: &mut CalcState) {
    s.x = s.y;
    s.y = s.z;
    s.z = s.t;
}
fn lift(s: &mut CalcState) {
    s.t = s.z;
    s.z = s.y;
    s.y = s.x;
}

fn to_rad(s: &CalcState, v: f64) -> f64 {
    match s.angle_mode.as_str() {
        "deg" => v * std::f64::consts::PI / 180.0,
        "grad" => v * std::f64::consts::PI / 200.0,
        _ => v,
    }
}
fn from_rad(s: &CalcState, v: f64) -> f64 {
    match s.angle_mode.as_str() {
        "deg" => v * 180.0 / std::f64::consts::PI,
        "grad" => v * 200.0 / std::f64::consts::PI,
        _ => v,
    }
}

/// Run a named command (or operator). Commits any pending entry first, then
/// arms stack lift for the next number (XRPN clears nolift after a command).
#[cfg_attr(feature = "uniffi", uniffi::export)]
pub fn execute(mut state: CalcState, command: String) -> StepResult {
    commit_entry(&mut state);
    let cmd = command.trim();

    // Alpha-register entry (XRPN string literals, case + spaces preserved):
    //   "text"        set Alpha
    //   >"text"       append to Alpha
    //   "|text"       append to Alpha
    //   "|-text"      append to Alpha, as an HP-41 listing prints it
    if let Some(rest) = alpha_append(cmd) {
        state.alpha.push_str(rest.trim_end_matches('"'));
        state.lift_enabled = true;
        return StepResult { state, error: None };
    }
    if cmd.starts_with('"') {
        state.alpha = cmd.trim_matches('"').to_string();
        state.lift_enabled = true;
        return StepResult { state, error: None };
    }

    let (name, arg) = split_arg(cmd);
    let name = canon(&name).to_string();
    let mut err: Option<String> = None;

    match name.as_str() {
        // ----- arithmetic
        "+" | "add" => {
            state.l = state.x;
            state.x = state.y + state.x;
            dropy(&mut state);
        }
        "-" | "subtract" => {
            state.l = state.x;
            state.x = state.y - state.x;
            dropy(&mut state);
        }
        "*" | "multiply" => {
            state.l = state.x;
            state.x = state.y * state.x;
            dropy(&mut state);
        }
        "/" | "divide" => {
            if state.x == 0.0 {
                err = Some("Division by zero".into());
            } else {
                state.l = state.x;
                state.x = state.y / state.x;
                dropy(&mut state);
            }
        }
        "pow" => {
            state.l = state.x;
            state.x = state.y.powf(state.x);
            dropy(&mut state);
        }
        "root" => {
            // X-th root of Y.
            state.l = state.x;
            state.x = state.y.powf(1.0 / state.x);
            dropy(&mut state);
        }
        "mod" => {
            state.l = state.x;
            state.x = state.y.rem_euclid(state.x);
            dropy(&mut state);
        }
        "percent" | "%" => {
            state.l = state.x;
            state.x = state.x * state.y / 100.0;
            dropy(&mut state);
        }
        "percentch" | "perch" => {
            state.l = state.x;
            state.x = 100.0 * ((state.x - state.y) / state.y);
            dropy(&mut state);
        }
        // ----- unary
        "sqrt" => unary(&mut state, |v| v.sqrt()),
        "sqr" => unary(&mut state, |v| v * v),
        "cube" => unary(&mut state, |v| v * v * v),
        "recip" => unary(&mut state, |v| 1.0 / v),
        "abs" => unary(&mut state, |v| v.abs()),
        "chs" => state.x = -state.x,
        "int" => unary(&mut state, |v| v.trunc()),
        "frc" => unary(&mut state, |v| v - v.trunc()),
        "sign" => unary(&mut state, |v| if v > 0.0 { 1.0 } else if v < 0.0 { -1.0 } else { 0.0 }),
        "rnd" => {
            let scale = 10f64.powi(state.decimals);
            unary(&mut state, |v| (v * scale).round() / scale);
        }
        "fact" => {
            let n = state.x;
            if n < 0.0 || n.fract() != 0.0 || n > 170.0 {
                err = Some("Factorial domain".into());
            } else {
                state.l = state.x;
                let mut acc = 1.0f64;
                let mut k = 2.0;
                while k <= n {
                    acc *= k;
                    k += 1.0;
                }
                state.x = acc;
            }
        }
        "pi" => {
            push_value(&mut state, std::f64::consts::PI);
        }
        // ----- logs / exp
        "ln" => unary(&mut state, |v| v.ln()),
        "log" => unary(&mut state, |v| v.log10()),
        "exp" => unary(&mut state, |v| v.exp()),
        "tenx" => unary(&mut state, |v| 10f64.powf(v)),
        "ln1x" => unary(&mut state, |v| (1.0 + v).ln()),
        "expx1" => unary(&mut state, |v| v.exp() - 1.0),
        // ----- trig
        "sin" => { let r = to_rad(&state, state.x); state.l = state.x; state.x = r.sin(); }
        "cos" => { let r = to_rad(&state, state.x); state.l = state.x; state.x = r.cos(); }
        "tan" => { let r = to_rad(&state, state.x); state.l = state.x; state.x = r.tan(); }
        "asin" => { state.l = state.x; state.x = from_rad(&state, state.x.asin()); }
        "acos" => { state.l = state.x; state.x = from_rad(&state, state.x.acos()); }
        "atan" => { state.l = state.x; state.x = from_rad(&state, state.x.atan()); }
        "deg" => state.angle_mode = "deg".into(),
        "rad" => state.angle_mode = "rad".into(),
        "grad" => state.angle_mode = "grad".into(),
        "r_d" => unary(&mut state, |v| v * 180.0 / std::f64::consts::PI),
        "d_r" => unary(&mut state, |v| v * std::f64::consts::PI / 180.0),
        "hms" => {
            state.l = state.x;
            let v = state.x;
            let h = v.trunc();
            let st = v * 3600.0;
            let m = (st / 60.0 - h * 60.0).trunc();
            let s = st - (m * 60.0 + h * 3600.0);
            state.x = h + m / 100.0 + s / 10000.0;
        }
        "hr" => {
            state.l = state.x;
            let v = state.x;
            let h = v.trunc();
            let frac = v - h;
            let mm = (frac * 100.0).trunc();
            let ss = ((v * 100.0).fract() * 100.0 * 10000.0).round() / 10000.0;
            let secs = h * 3600.0 + mm * 60.0 + ss;
            state.x = secs / 3600.0;
        }
        "p_r" => {
            // polar (r=X, theta=Y) -> rect (x=X, y=Y)
            state.l = state.x;
            let r = state.x;
            let theta = to_rad(&state, state.y);
            state.x = r * theta.cos();
            state.y = r * theta.sin();
        }
        "r_p" => {
            // rect (x=X, y=Y) -> polar (r=X, theta=Y)
            state.l = state.x;
            let xx = state.x;
            let yy = state.y;
            state.x = (xx * xx + yy * yy).sqrt();
            let mut th = (yy).atan2(xx);
            if th < 0.0 {
                th += 2.0 * std::f64::consts::PI;
            }
            state.y = from_rad(&state, th);
        }
        // ----- stack
        "enter" => return StepResult { state: key_enter(state), error: None },
        "swap" | "xy" => std::mem::swap(&mut state.x, &mut state.y),
        // X<> nn: swap X with a register (or with Y, Z, T, L).
        "x<>" => {
            if let Some(r) = arg {
                let (old_x, v) = (state.x, recall_reg(&state, &r));
                put_reg(&mut state, &r, old_x);
                state.x = v;
            } else {
                err = Some("X<> needs a register".into());
            }
        }
        "rdn" => {
            let x = state.x;
            state.x = state.y;
            state.y = state.z;
            state.z = state.t;
            state.t = x;
        }
        "rup" => {
            let t = state.t;
            state.t = state.z;
            state.z = state.y;
            state.y = state.x;
            state.x = t;
        }
        "drop" => drop_full(&mut state),
        "dropy" => dropy(&mut state),
        "lastx" => {
            lift(&mut state);
            state.x = state.l;
        }
        "clx" => return StepResult { state: key_clx(state), error: None },
        "clst" | "clear" => {
            state.x = 0.0; state.y = 0.0; state.z = 0.0; state.t = 0.0;
        }
        // ----- registers
        "sto" => {
            if let Some(r) = arg {
                store_reg(&mut state, &r);
            } else {
                err = Some("STO needs a register".into());
            }
        }
        "rcl" => {
            if let Some(r) = arg {
                let v = recall_reg(&state, &r);
                push_value(&mut state, v);
            } else {
                err = Some("RCL needs a register".into());
            }
        }
        "stplus" | "st+" => reg_arith(&mut state, arg, |a, b| a + b, &mut err),
        "stsubtract" | "st-" => reg_arith(&mut state, arg, |a, b| a - b, &mut err),
        "stmultiply" | "st*" => reg_arith(&mut state, arg, |a, b| a * b, &mut err),
        "stdivide" | "st/" => reg_arith(&mut state, arg, |a, b| a / b, &mut err),
        "clrg" => state.reg.clear(),
        // ----- statistics
        "splus" => stat_accumulate(&mut state, 1.0),
        "sminus" => stat_accumulate(&mut state, -1.0),
        "cls" => {
            for k in 0..6 {
                state.reg.remove(&(state.stat_base + k).to_string());
            }
        }
        "mean" => {
            let n = reg_at(&state, state.stat_base + 1);
            if n == 0.0 {
                err = Some("No data".into());
            } else {
                let sx = reg_at(&state, state.stat_base);
                let sy = reg_at(&state, state.stat_base + 3);
                lift(&mut state);
                state.x = sx / n;
                state.y = sy / n;
            }
        }
        "sdev" => {
            let n = reg_at(&state, state.stat_base + 1);
            if n < 2.0 {
                err = Some("Need 2+ data points".into());
            } else {
                let sx = reg_at(&state, state.stat_base);
                let sx2 = reg_at(&state, state.stat_base + 2);
                let sy = reg_at(&state, state.stat_base + 3);
                let sy2 = reg_at(&state, state.stat_base + 4);
                let vx = (sx2 - sx * sx / n) / (n - 1.0);
                let vy = (sy2 - sy * sy / n) / (n - 1.0);
                lift(&mut state);
                state.x = vx.max(0.0).sqrt();
                state.y = vy.max(0.0).sqrt();
            }
        }
        // ----- display modes
        "fix" => { state.decimals = arg_int(arg, 4); state.threshold = 9; state.grouping = 1; }
        "sci" => { state.decimals = arg_int(arg, 4); state.threshold = 6; state.grouping = 1; }
        "eng" => { state.decimals = arg_int(arg, 4); state.threshold = 6; state.grouping = 3; }
        "sep" => toggle_flag(&mut state, "29"),
        "dot" => toggle_flag(&mut state, "28"),
        // ----- flags
        "sf" => { if let Some(f) = arg { state.flags.insert(norm_flag(&f), true); } }
        "cf" => { if let Some(f) = arg { state.flags.insert(norm_flag(&f), false); } }
        // ----- base conversion (via alpha display)
        "dechex" => state.alpha = format!("{:X}", state.x as i64),
        "decbin" => state.alpha = format!("{:b}", state.x as i64),
        "decoct" => state.alpha = format!("{:o}", state.x as i64),
        "hexdec" => {
            if let Some(a) = arg.or_else(|| nonempty(&state.alpha)) {
                match i64::from_str_radix(a.trim_start_matches("0x"), 16) {
                    Ok(v) => push_value(&mut state, v as f64),
                    Err(_) => err = Some("Bad hex".into()),
                }
            } else { err = Some("HEXDEC needs hex in Alpha or as arg".into()); }
        }
        "bindec" => {
            if let Some(a) = arg.or_else(|| nonempty(&state.alpha)) {
                match i64::from_str_radix(&a, 2) {
                    Ok(v) => push_value(&mut state, v as f64),
                    Err(_) => err = Some("Bad binary".into()),
                }
            } else { err = Some("BINDEC needs binary in Alpha or as arg".into()); }
        }
        "octdec" => {
            if let Some(a) = arg.or_else(|| nonempty(&state.alpha)) {
                match i64::from_str_radix(&a, 8) {
                    Ok(v) => push_value(&mut state, v as f64),
                    Err(_) => err = Some("Bad octal".into()),
                }
            } else { err = Some("OCTDEC needs octal in Alpha or as arg".into()); }
        }
        // ----- alpha register manipulation
        "cla" => state.alpha = String::new(),
        // AVIEW is a no-op here: the Alpha register is always shown on screen.
        "aview" | "avi" | "aviewc" | "aon" | "aoff" | "prompt" | "pse" => {}
        // Sound, display and printer commands of the HP-41 do nothing here.
        "tone" | "beep" | "cld" | "adv" => {}
        "aleng" => {
            let n = state.alpha.chars().count() as f64;
            push_value(&mut state, n);
        }
        "ashf" => {
            state.alpha = state.alpha.chars().skip(6).collect();
        }
        "arot" => {
            let chars: Vec<char> = state.alpha.chars().collect();
            let len = chars.len();
            if len > 0 {
                let n = state.x as i64;
                // positive => rotate left, negative => rotate right
                let shift = (((n % len as i64) + len as i64) % len as i64) as usize;
                let rotated: String =
                    chars[shift..].iter().chain(chars[..shift].iter()).collect();
                state.alpha = rotated;
            }
        }
        "xtoa" => {
            let code = state.x as u32;
            match char::from_u32(code) {
                Some(c) => state.alpha.push(c),
                None => err = Some("XTOA: X is not a valid character code".into()),
            }
        }
        "atox" => {
            match state.alpha.chars().next() {
                Some(c) => {
                    push_value(&mut state, c as u32 as f64);
                    state.alpha = state.alpha.chars().skip(1).collect();
                }
                // An empty Alpha gives 0, as on the HP-41. Programs loop on that.
                None => push_value(&mut state, 0.0),
            }
        }
        "anum" => match first_number(&state.alpha, comma(&state)) {
            Some(v) => push_value(&mut state, v),
            None => err = Some("ANUM: no number in Alpha".into()),
        },
        "posa" => {
            // Numeric-stack variant: X is a character code; report its 1-based
            // position in Alpha (0 if absent). The string-pattern form needs
            // text on the stack, which the numeric stack can't hold.
            state.l = state.x;
            let code = state.x as u32;
            let pos = char::from_u32(code)
                .and_then(|target| state.alpha.chars().position(|c| c == target))
                .map(|i| i as f64 + 1.0)
                .unwrap_or(0.0);
            state.x = pos;
        }
        "asto" => {
            if let Some(r) = arg.as_deref() {
                match r.to_lowercase().as_str() {
                    "x" | "y" | "z" | "t" | "l" => {
                        err = Some("ASTO to the stack needs Alpha on the stack (unsupported on the numeric stack)".into());
                    }
                    _ => {
                        state.areg.insert(r.to_string(), state.alpha.clone());
                    }
                }
            } else {
                err = Some("ASTO needs a register".into());
            }
        }
        "arcl" => {
            if let Some(r) = arg.as_deref() {
                match r.to_lowercase().as_str() {
                    "x" => state.alpha.push_str(&fmt(&state, state.x)),
                    "y" => state.alpha.push_str(&fmt(&state, state.y)),
                    "z" => state.alpha.push_str(&fmt(&state, state.z)),
                    "t" => state.alpha.push_str(&fmt(&state, state.t)),
                    "l" => state.alpha.push_str(&fmt(&state, state.l)),
                    _ => {
                        if let Some(s) = state.areg.get(r) {
                            let s = s.clone();
                            state.alpha.push_str(&s);
                        } else if let Some(&v) = state.reg.get(&reg_key(r)) {
                            state.alpha.push_str(&fmt(&state, v));
                        } else {
                            err = Some(format!("ARCL: register {} is empty", r));
                        }
                    }
                }
            } else {
                err = Some("ARCL needs a register".into());
            }
        }
        _ => {
            err = Some(format!("No such command: {}", name));
        }
    }

    // After a command, re-arm stack lift — except Σ+/Σ- which set nolift so a
    // following number overwrites the count they left in X.
    let nolift_after = matches!(name.as_str(), "splus" | "sminus");
    state.lift_enabled = !nolift_after;
    StepResult { state, error: err }
}

// ---------------------------------------------------------------- helpers

fn unary(s: &mut CalcState, f: impl Fn(f64) -> f64) {
    s.l = s.x;
    s.x = f(s.x);
}

/// Push a freshly-produced value, lifting the stack (a value-producing command
/// like PI or RCL lifts unless we are immediately after ENTER).
fn push_value(s: &mut CalcState, v: f64) {
    if s.lift_enabled {
        lift(s);
    }
    s.x = v;
}

fn split_arg(cmd: &str) -> (String, Option<String>) {
    let mut parts = cmd.split_whitespace();
    let name = parts.next().unwrap_or("").to_lowercase();
    let rest: Vec<&str> = parts.collect();
    if rest.is_empty() {
        (name, None)
    } else {
        (name, Some(rest.join(" ")))
    }
}

fn arg_int(arg: Option<String>, default: i32) -> i32 {
    arg.and_then(|a| a.trim().parse::<i32>().ok()).unwrap_or(default).clamp(0, 11)
}

/// HP-41 spellings of the commands, so a listing keyed on a real HP-41 runs
/// unchanged. `name` is already lowercase (a capital sigma lowercases to
/// either small sigma, depending on where it stands).
pub(crate) fn canon(name: &str) -> &str {
    match name {
        "x<>y" => "swap",
        "1/x" => "recip",
        "x^2" | "x\u{2191}2" => "sqr",
        "y^x" | "y\u{2191}x" => "pow",
        "e^x" | "e\u{2191}x" => "exp",
        "10^x" | "10\u{2191}x" => "tenx",
        "e^x-1" | "e\u{2191}x-1" => "expx1",
        "ln1+x" => "ln1x",
        "enter^" | "enter\u{2191}" => "enter",
        "r^" | "r\u{2191}" => "rup",
        "\u{3c3}+" => "splus",
        "\u{3c3}-" => "sminus",
        "cl\u{3c3}" | "cl\u{3c2}" => "cls",
        "%ch" => "percentch",
        "d-r" => "d_r",
        "r-d" => "r_d",
        "p-r" => "p_r",
        "r-p" => "r_p",
        "x=0?" => "xeq0",
        "x\u{2260}0?" | "x#0?" | "x!=0?" | "x<>0?" => "xneq0",
        "x<0?" => "xlt0",
        "x>0?" => "xgt0",
        "x<=0?" | "x\u{2264}0?" => "xlteq0",
        "x>=0?" | "x\u{2265}0?" => "xgteq0",
        "x=y?" => "xeqy",
        "x\u{2260}y?" | "x#y?" | "x!=y?" | "x<>y?" => "xneqy",
        "x<y?" => "xlty",
        "x>y?" => "xgty",
        "x<=y?" | "x\u{2264}y?" => "xlteqy",
        "x>=y?" | "x\u{2265}y?" => "xgteqy",
        other => other,
    }
}

/// The text after an Alpha append marker, if the line starts with one:
/// `>"`, `"|-` and its one-character forms, or `"|`.
pub(crate) fn alpha_append(line: &str) -> Option<&str> {
    [">\"", "\"|-", "\"\u{22a2}", "\"\u{251c}", "\"|"].iter().find_map(|p| line.strip_prefix(p))
}

pub(crate) fn norm_flag(f: &str) -> String {
    f.trim().parse::<i32>().map(|n| n.to_string()).unwrap_or_else(|_| f.trim().to_string())
}

fn toggle_flag(s: &mut CalcState, f: &str) {
    let cur = *s.flags.get(f).unwrap_or(&false);
    s.flags.insert(f.to_string(), !cur);
}

fn nonempty(s: &str) -> Option<String> {
    if s.is_empty() { None } else { Some(s.to_string()) }
}

/// First numeric token in a string (ANUM). Mirrors desktop XRPN: grab the
/// first run of digits / separators / exponent, then parse (comma→dot when
/// comma is the decimal separator).
fn first_number(a: &str, comma_decimal: bool) -> Option<f64> {
    let mut tok = String::new();
    for c in a.chars() {
        if c.is_ascii_digit() || c == '.' || c == ',' || c == 'e' || c == 'E' {
            tok.push(c);
        } else if !tok.is_empty() {
            break;
        }
    }
    if tok.is_empty() {
        return None;
    }
    let norm = if comma_decimal { tok.replace(',', ".") } else { tok };
    norm.parse::<f64>().ok()
}

fn reg_key(r: &str) -> String {
    // Accept "5", "05", " 5 " — normalise to plain integer string.
    let trimmed = r.trim().trim_start_matches('0');
    if trimmed.is_empty() { "0".to_string() } else { trimmed.to_string() }
}

fn store_reg(s: &mut CalcState, r: &str) {
    let x = s.x;
    put_reg(s, r, x);
}

/// Write `v` to a numbered or named register, or to X, Y, Z, T or L.
fn put_reg(s: &mut CalcState, r: &str, v: f64) {
    match r.trim().to_lowercase().as_str() {
        "x" => s.x = v,
        "y" => s.y = v,
        "z" => s.z = v,
        "t" => s.t = v,
        "l" => s.l = v,
        _ => { s.reg.insert(reg_key(r), v); }
    }
}

pub(crate) fn recall_reg(s: &CalcState, r: &str) -> f64 {
    match r.trim().to_lowercase().as_str() {
        "x" => s.x,
        "y" => s.y,
        "z" => s.z,
        "t" => s.t,
        "l" => s.l,
        _ => *s.reg.get(&reg_key(r)).unwrap_or(&0.0),
    }
}

fn reg_arith(
    s: &mut CalcState,
    arg: Option<String>,
    f: impl Fn(f64, f64) -> f64,
    err: &mut Option<String>,
) {
    match arg {
        Some(r) => {
            let nv = f(recall_reg(s, &r), s.x);
            put_reg(s, &r, nv);
        }
        None => *err = Some("Needs a register".into()),
    }
}

fn reg_at(s: &CalcState, idx: i32) -> f64 {
    *s.reg.get(&idx.to_string()).unwrap_or(&0.0)
}

/// Σ+ / Σ- using XRPN's register layout from stat_base:
/// b: Σx, b+1: n, b+2: Σx², b+3: Σy, b+4: Σy², b+5: Σxy.
fn stat_accumulate(s: &mut CalcState, dir: f64) {
    let b = s.stat_base;
    let x = s.x;
    let y = s.y;
    let upd = |s: &mut CalcState, idx: i32, dv: f64| {
        let k = idx.to_string();
        let cur = *s.reg.get(&k).unwrap_or(&0.0);
        s.reg.insert(k, cur + dv);
    };
    upd(s, b, dir * x);
    upd(s, b + 1, dir);
    upd(s, b + 2, dir * x * x);
    upd(s, b + 3, dir * y);
    upd(s, b + 4, dir * y * y);
    upd(s, b + 5, dir * x * y);
    // X becomes n (the count), matching HP-41 Σ+ leaving n in X. The caller
    // (execute) handles the nolift after Σ+/Σ-.
    s.l = s.x;
    s.x = reg_at(s, b + 1);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(mut st: CalcState, cmds: &[&str]) -> CalcState {
        for c in cmds {
            st = execute(st, c.to_string()).state;
        }
        st
    }
    fn typed(mut st: CalcState, num: &str) -> CalcState {
        for ch in num.chars() {
            st = match ch {
                '.' => key_dot(st),
                '-' => key_chs(st),
                _ => key_digit(st, ch.to_string()),
            };
        }
        st
    }

    #[test]
    fn addition() {
        let mut s = new_state();
        s = typed(s, "5");
        s = key_enter(s);
        s = typed(s, "3");
        s = execute(s, "+".into()).state;
        assert_eq!(s.x, 8.0);
    }

    #[test]
    fn alpha_entry_set_and_append() {
        let mut s = new_state();
        s = run(s, &["\"Hello\""]);
        assert_eq!(s.alpha, "Hello");
        s = run(s, &[">\" World\""]); // append form
        assert_eq!(s.alpha, "Hello World");
        s = run(s, &["\"|!\""]); // other append form
        assert_eq!(s.alpha, "Hello World!");
        s = run(s, &["cla"]);
        assert_eq!(s.alpha, "");
    }

    #[test]
    fn alpha_aleng_arot_ashf() {
        let mut s = new_state();
        s = run(s, &["\"ABCDE\"", "aleng"]);
        assert_eq!(s.x, 5.0);
        // rotate left by 2 -> CDEAB
        s = typed(s, "2");
        s = run(s, &["arot"]);
        assert_eq!(s.alpha, "CDEAB");
        // shift drops first 6 (here all 5) -> empty
        s = run(s, &["\"ABCDEFGH\"", "ashf"]);
        assert_eq!(s.alpha, "GH");
    }

    #[test]
    fn alpha_xtoa_atox_anum() {
        let mut s = new_state();
        s = typed(s, "65"); // 'A'
        s = run(s, &["xtoa"]);
        assert_eq!(s.alpha, "A");
        // atox pops the 'A' code back to X
        s = run(s, &["atox"]);
        assert_eq!(s.x, 65.0);
        assert_eq!(s.alpha, "");
        // anum pulls the first number out of free text
        s = run(s, &["\"qty 42 left\"", "anum"]);
        assert_eq!(s.x, 42.0);
    }

    #[test]
    fn alpha_asto_arcl_roundtrip_and_number() {
        let mut s = new_state();
        // store/recall an alpha string via a numbered register
        s = run(s, &["\"Tag\"", "asto 5", "cla", "arcl 5"]);
        assert_eq!(s.alpha, "Tag");
        // ARCL of a numeric register appends the formatted value
        s = typed(s, "3");
        s = run(s, &["sto 7", "cla", "\"n=\"", "arcl 7"]);
        assert_eq!(s.alpha, "n=3,0000"); // comma decimal, FIX 4 default
    }

    #[test]
    fn alpha_posa_charcode() {
        let mut s = new_state();
        s = run(s, &["\"ABC\""]);
        s = typed(s, "66"); // 'B'
        s = run(s, &["posa"]);
        assert_eq!(s.x, 2.0); // 1-based
    }

    #[test]
    fn classic_rpn_chain() {
        // (5 + 3) * 2 = 16
        let mut s = new_state();
        s = typed(s, "5");
        s = key_enter(s);
        s = typed(s, "3");
        s = execute(s, "+".into()).state;
        s = typed(s, "2");
        s = execute(s, "*".into()).state;
        assert_eq!(s.x, 16.0);
    }

    #[test]
    fn enter_disables_lift_then_rearms() {
        // 2 ENTER ENTER -> X=2,Y=2,Z=2 ; then 3 (overwrites X, no lift) -> X=3
        let mut s = new_state();
        s = typed(s, "2");
        s = key_enter(s);
        s = key_enter(s);
        assert_eq!((s.x, s.y, s.z), (2.0, 2.0, 2.0));
        s = typed(s, "3");
        assert_eq!(s.x, 3.0);
        assert_eq!(s.y, 2.0);
    }

    #[test]
    fn lift_after_op() {
        // 5 ENTER 3 + (=8) then 2 should lift: X=2, Y=8
        let mut s = new_state();
        s = typed(s, "5");
        s = key_enter(s);
        s = typed(s, "3");
        s = execute(s, "+".into()).state;
        s = typed(s, "2");
        assert_eq!(s.x, 2.0);
        assert_eq!(s.y, 8.0);
    }

    #[test]
    fn last_x() {
        // 5 ENTER 3 + -> lastx should bring back 3
        let mut s = new_state();
        s = typed(s, "5");
        s = key_enter(s);
        s = typed(s, "3");
        s = execute(s, "+".into()).state;
        s = execute(s, "lastx".into()).state;
        assert_eq!(s.x, 3.0);
        assert_eq!(s.y, 8.0);
    }

    #[test]
    fn divide_by_zero_errors_and_preserves() {
        let mut s = new_state();
        s = typed(s, "5");
        s = key_enter(s);
        s = typed(s, "0");
        let r = execute(s, "/".into());
        assert!(r.error.is_some());
        assert_eq!(r.state.x, 0.0); // X untouched (still the 0 we entered)
    }

    #[test]
    fn registers() {
        let mut s = new_state();
        s = typed(s, "42");
        s = execute(s, "sto 5".into()).state;
        s = key_clx(s);
        s = execute(s, "rcl 5".into()).state;
        assert_eq!(s.x, 42.0);
        // st+ accumulates
        s = typed(s, "8");
        s = execute(s, "st+ 5".into()).state;
        assert_eq!(*s.reg.get("5").unwrap(), 50.0);
    }

    #[test]
    fn trig_deg() {
        let mut s = new_state();
        s = typed(s, "90");
        s = execute(s, "sin".into()).state;
        assert!((s.x - 1.0).abs() < 1e-9);
    }

    #[test]
    fn sqrt_and_lastx_chain() {
        let mut s = new_state();
        s = typed(s, "16");
        s = execute(s, "sqrt".into()).state;
        assert_eq!(s.x, 4.0);
        assert_eq!(s.l, 16.0);
    }

    #[test]
    fn pi_lifts() {
        let mut s = new_state();
        s = typed(s, "5");
        s = execute(s, "pi".into()).state;
        assert!((s.x - std::f64::consts::PI).abs() < 1e-12);
        assert_eq!(s.y, 5.0); // 5 lifted up
    }

    #[test]
    fn hms_roundtrip() {
        // 1.5 hours -> 1.3000 (1h30m00s) -> back to 1.5
        let mut s = new_state();
        s = typed(s, "1.5");
        s = execute(s, "hms".into()).state;
        assert!((s.x - 1.30).abs() < 1e-6, "got {}", s.x);
        s = execute(s, "hr".into()).state;
        assert!((s.x - 1.5).abs() < 1e-6, "got {}", s.x);
    }

    #[test]
    fn stats_mean() {
        let mut s = new_state();
        // data points x: 2,4,6 (y=0). mean x = 4.
        for v in ["2", "4", "6"] {
            s = typed(s, v);
            s = execute(s, "splus".into()).state;
        }
        s = execute(s, "mean".into()).state;
        assert!((s.x - 4.0).abs() < 1e-9);
    }

    #[test]
    fn unknown_command_errors() {
        let s = new_state();
        let r = execute(s, "frobnicate".into());
        assert!(r.error.is_some());
    }
}
