//! Lua 5.4 byte patterns, following lstrlib.c's matcher and capture rollback.

// Adapted from Lua 5.4.9 lstrlib.c:
// Copyright (C) 1994-2026 Lua.org, PUC-Rio.
//
// Permission is hereby granted, free of charge, to any person obtaining
// a copy of this software and associated documentation files (the
// "Software"), to deal in the Software without restriction, including
// without limitation the rights to use, copy, modify, merge, publish,
// distribute, sublicense, and/or sell copies of the Software, and to
// permit persons to whom the Software is furnished to do so, subject to
// the following conditions:
//
// The above copyright notice and this permission notice shall be
// included in all copies or substantial portions of the Software.
//
// THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND,
// EXPRESS OR IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF
// MERCHANTABILITY, FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT.
// IN NO EVENT SHALL THE AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY
// CLAIM, DAMAGES OR OTHER LIABILITY, WHETHER IN AN ACTION OF CONTRACT,
// TORT OR OTHERWISE, ARISING FROM, OUT OF OR IN CONNECTION WITH THE
// SOFTWARE OR THE USE OR OTHER DEALINGS IN THE SOFTWARE.

use mlua::{Function, Lua, MultiValue, Result, Value, ffi};

// Enough for linear scans of MiB-sized strings, while stopping exponential
// backtracking well before the instruction watchdog could otherwise help.
const MAX_PATTERN_STEPS: usize = 20_000_000;
const MAX_CAPTURES: usize = 32;
const MAX_DEPTH: usize = 200;
const UNFINISHED: isize = -1;
const POSITION: isize = -2;

fn error(message: impl Into<String>) -> mlua::Error {
    mlua::Error::RuntimeError(message.into())
}

struct Budget(usize);

impl Budget {
    fn step(&mut self) -> Result<()> {
        if self.0 == 0 {
            return Err(error("pattern too complex"));
        }
        self.0 -= 1;
        Ok(())
    }
}

#[derive(Clone, Copy, Default)]
struct Capture {
    start: usize,
    len: isize,
}

struct Matcher<'a> {
    subject: &'a [u8],
    pattern: &'a [u8],
    captures: [Capture; MAX_CAPTURES],
    level: usize,
    budget: &'a mut Budget,
}

impl<'a> Matcher<'a> {
    fn new(subject: &'a [u8], pattern: &'a [u8], budget: &'a mut Budget) -> Self {
        Self {
            subject,
            pattern,
            captures: [Capture::default(); MAX_CAPTURES],
            level: 0,
            budget,
        }
    }

    fn class_end(&mut self, mut p: usize) -> Result<usize> {
        let first = self.pattern[p];
        p += 1;
        match first {
            b'%' => {
                if p == self.pattern.len() {
                    Err(error("malformed pattern (ends with '%')"))
                } else {
                    Ok(p + 1)
                }
            }
            b'[' => {
                if self.pattern.get(p) == Some(&b'^') {
                    p += 1;
                }
                loop {
                    self.budget.step()?;
                    let c = *self
                        .pattern
                        .get(p)
                        .ok_or_else(|| error("malformed pattern (missing ']')"))?;
                    p += 1;
                    if c == b'%' && p < self.pattern.len() {
                        p += 1;
                    }
                    if self.pattern.get(p) == Some(&b']') {
                        return Ok(p + 1);
                    }
                }
            }
            _ => Ok(p),
        }
    }

    fn bracket(&mut self, c: u8, mut p: usize, end: usize) -> Result<bool> {
        p += 1;
        let positive = self.pattern.get(p) != Some(&b'^');
        if !positive {
            p += 1;
        }
        while p < end {
            self.budget.step()?;
            let matches = if self.pattern[p] == b'%' {
                p += 1;
                class(c, self.pattern[p])
            } else if self.pattern.get(p + 1) == Some(&b'-') && p + 2 < end {
                p += 2;
                self.pattern[p - 2] <= c && c <= self.pattern[p]
            } else {
                self.pattern[p] == c
            };
            if matches {
                return Ok(positive);
            }
            p += 1;
        }
        Ok(!positive)
    }

    fn single(&mut self, s: usize, p: usize, end: usize) -> Result<bool> {
        let Some(&c) = self.subject.get(s) else {
            return Ok(false);
        };
        match self.pattern[p] {
            b'.' => Ok(true),
            b'%' => Ok(class(c, self.pattern[p + 1])),
            b'[' => self.bracket(c, p, end - 1),
            literal => Ok(c == literal),
        }
    }

    fn max_expand(
        &mut self,
        s: usize,
        p: usize,
        end: usize,
        depth: usize,
    ) -> Result<Option<usize>> {
        let mut i = 0;
        while self.single(s + i, p, end)? {
            self.budget.step()?;
            i += 1;
        }
        loop {
            self.budget.step()?;
            if let Some(result) = self.matches(s + i, end + 1, depth)? {
                return Ok(Some(result));
            }
            if i == 0 {
                return Ok(None);
            }
            i -= 1;
        }
    }

    fn min_expand(
        &mut self,
        mut s: usize,
        p: usize,
        end: usize,
        depth: usize,
    ) -> Result<Option<usize>> {
        loop {
            self.budget.step()?;
            if let Some(result) = self.matches(s, end + 1, depth)? {
                return Ok(Some(result));
            }
            if !self.single(s, p, end)? {
                return Ok(None);
            }
            s += 1;
        }
    }

    fn matches(&mut self, mut s: usize, mut p: usize, depth: usize) -> Result<Option<usize>> {
        self.budget.step()?;
        if depth == MAX_DEPTH {
            return Err(error("pattern too complex"));
        }
        let depth = depth + 1;
        loop {
            // Lua tail calls do not consume recursion depth, but still do work.
            self.budget.step()?;
            if p == self.pattern.len() {
                return Ok(Some(s));
            }
            match self.pattern[p] {
                b'(' => {
                    if self.level == MAX_CAPTURES {
                        return Err(error("too many captures"));
                    }
                    let position = self.pattern.get(p + 1) == Some(&b')');
                    self.captures[self.level] = Capture {
                        start: s,
                        len: if position { POSITION } else { UNFINISHED },
                    };
                    self.level += 1;
                    let result = self.matches(s, p + if position { 2 } else { 1 }, depth)?;
                    if result.is_none() {
                        self.level -= 1;
                    }
                    return Ok(result);
                }
                b')' => {
                    let index = (0..self.level)
                        .rev()
                        .find(|&i| self.captures[i].len == UNFINISHED)
                        .ok_or_else(|| error("invalid pattern capture"))?;
                    self.captures[index].len = (s - self.captures[index].start) as isize;
                    let result = self.matches(s, p + 1, depth)?;
                    if result.is_none() {
                        self.captures[index].len = UNFINISHED;
                    }
                    return Ok(result);
                }
                b'$' if p + 1 == self.pattern.len() => {
                    return Ok((s == self.subject.len()).then_some(s));
                }
                b'%' => match self.pattern.get(p + 1).copied() {
                    Some(b'b') => {
                        if p + 3 >= self.pattern.len() {
                            return Err(error("malformed pattern (missing arguments to '%b')"));
                        }
                        let open = self.pattern[p + 2];
                        let close = self.pattern[p + 3];
                        if self.subject.get(s).copied().unwrap_or(0) != open {
                            return Ok(None);
                        }
                        let mut count = 1usize;
                        s += 1;
                        while s < self.subject.len() {
                            self.budget.step()?;
                            if self.subject[s] == close {
                                count -= 1;
                                if count == 0 {
                                    break;
                                }
                            } else if self.subject[s] == open {
                                count += 1;
                            }
                            s += 1;
                        }
                        if count != 0 {
                            return Ok(None);
                        }
                        s += 1;
                        p += 4;
                        continue;
                    }
                    Some(b'f') => {
                        p += 2;
                        if self.pattern.get(p) != Some(&b'[') {
                            return Err(error("missing '[' after '%f' in pattern"));
                        }
                        let end = self.class_end(p)?;
                        let previous = if s == 0 { 0 } else { self.subject[s - 1] };
                        let current = self.subject.get(s).copied().unwrap_or(0);
                        if self.bracket(previous, p, end - 1)?
                            || !self.bracket(current, p, end - 1)?
                        {
                            return Ok(None);
                        }
                        p = end;
                        continue;
                    }
                    Some(digit @ b'0'..=b'9') => {
                        let index = digit.wrapping_sub(b'1') as usize;
                        if index >= self.level || self.captures[index].len == UNFINISHED {
                            return Err(error(format!("invalid capture index %{}", digit - b'0')));
                        }
                        let capture = self.captures[index];
                        let Ok(len) = usize::try_from(capture.len) else {
                            return Ok(None);
                        };
                        if len > self.subject.len() - s {
                            return Ok(None);
                        }
                        for i in 0..len {
                            self.budget.step()?;
                            if self.subject[capture.start + i] != self.subject[s + i] {
                                return Ok(None);
                            }
                        }
                        s += len;
                        p += 2;
                        continue;
                    }
                    _ => {}
                },
                _ => {}
            }
            let end = self.class_end(p)?;
            let suffix = self.pattern.get(end).copied();
            if !self.single(s, p, end)? {
                if matches!(suffix, Some(b'*' | b'?' | b'-')) {
                    p = end + 1;
                    continue;
                }
                return Ok(None);
            }
            match suffix {
                Some(b'?') => {
                    if let Some(result) = self.matches(s + 1, end + 1, depth)? {
                        return Ok(Some(result));
                    }
                    p = end + 1;
                }
                Some(b'+') => return self.max_expand(s + 1, p, end, depth),
                Some(b'*') => return self.max_expand(s, p, end, depth),
                Some(b'-') => return self.min_expand(s, p, end, depth),
                _ => {
                    s += 1;
                    p = end;
                }
            }
        }
    }

    fn capture(&self, lua: &Lua, index: usize, start: usize, end: usize) -> Result<Value> {
        if index >= self.level {
            if index == 0 {
                return Ok(Value::String(lua.create_string(&self.subject[start..end])?));
            }
            return Err(error(format!("invalid capture index %{}", index + 1)));
        }
        let capture = self.captures[index];
        match capture.len {
            UNFINISHED => Err(error("unfinished capture")),
            POSITION => Ok(Value::Integer((capture.start + 1) as i64)),
            len => Ok(Value::String(lua.create_string(
                &self.subject[capture.start..capture.start + len as usize],
            )?)),
        }
    }

    fn values(&self, lua: &Lua, start: usize, end: usize, whole: bool) -> Result<MultiValue> {
        (0..if whole { self.level.max(1) } else { self.level })
            .map(|i| self.capture(lua, i, start, end))
            .collect()
    }
}

unsafe extern "C" {
    fn isalpha(c: std::ffi::c_int) -> std::ffi::c_int;
    fn iscntrl(c: std::ffi::c_int) -> std::ffi::c_int;
    fn isdigit(c: std::ffi::c_int) -> std::ffi::c_int;
    fn isgraph(c: std::ffi::c_int) -> std::ffi::c_int;
    fn islower(c: std::ffi::c_int) -> std::ffi::c_int;
    fn ispunct(c: std::ffi::c_int) -> std::ffi::c_int;
    fn isspace(c: std::ffi::c_int) -> std::ffi::c_int;
    fn isupper(c: std::ffi::c_int) -> std::ffi::c_int;
    fn isalnum(c: std::ffi::c_int) -> std::ffi::c_int;
    fn isxdigit(c: std::ffi::c_int) -> std::ffi::c_int;
}

fn class(c: u8, cl: u8) -> bool {
    // SAFETY: C character classification accepts every unsigned char value.
    // Use the same CRT locale as native Lua, including changes by trusted VMs.
    let result = unsafe {
        match cl.to_ascii_lowercase() {
            b'a' => isalpha(c.into()) != 0,
            b'c' => iscntrl(c.into()) != 0,
            b'd' => isdigit(c.into()) != 0,
            b'g' => isgraph(c.into()) != 0,
            b'l' => islower(c.into()) != 0,
            b'p' => ispunct(c.into()) != 0,
            b's' => isspace(c.into()) != 0,
            b'u' => isupper(c.into()) != 0,
            b'w' => isalnum(c.into()) != 0,
            b'x' => isxdigit(c.into()) != 0,
            b'z' => c == 0,
            _ => return c == cl,
        }
    };
    if cl.is_ascii_lowercase() {
        result
    } else {
        !result
    }
}

fn initial(pos: i64, len: usize) -> usize {
    if pos > 0 {
        (pos - 1) as usize
    } else if pos == 0 || pos < -(len as i64) {
        0
    } else {
        (len as i64 + pos) as usize
    }
}

// The C facade validates arguments before invoking Rust.
struct Arguments {
    values: MultiValue,
}

impl Arguments {
    fn string(&self, lua: &Lua, index: usize) -> Result<mlua::LuaString> {
        lua.coerce_string(self.values.get(index - 1).cloned().unwrap_or(Value::Nil))?
            .ok_or_else(|| error("string expected"))
    }

    fn integer(&self, lua: &Lua, index: usize, default: i64) -> Result<i64> {
        let value = self.values.get(index - 1).cloned().unwrap_or(Value::Nil);
        if value == Value::Nil {
            return Ok(default);
        }
        lua.coerce_integer(value)?
            .ok_or_else(|| error("integer expected"))
    }
}

fn lua_type(value: &Value) -> &'static str {
    match value {
        Value::Integer(_) => "number",
        _ => value.type_name(),
    }
}

enum Outcome {
    Values(MultiValue),
    Raised(Value),
}

// The facade raises only after all Rust matcher state has been dropped.
fn response(lua: &Lua, result: Result<Outcome>) -> Result<MultiValue> {
    match result {
        Ok(Outcome::Values(mut values)) => {
            values.push_front(Value::Boolean(true));
            Ok(values)
        }
        Ok(Outcome::Raised(value)) => Ok(MultiValue::from_vec(vec![Value::Boolean(false), value])),
        Err(err) => {
            let message = match err {
                mlua::Error::RuntimeError(message) => message,
                other => other.to_string(),
            };
            Ok(MultiValue::from_vec(vec![
                Value::Boolean(false),
                Value::String(lua.create_string(message)?),
            ]))
        }
    }
}

fn find(lua: &Lua, args: MultiValue, is_find: bool) -> Result<MultiValue> {
    let args = Arguments { values: args };
    let subject = args.string(lua, 1)?;
    let pattern = args.string(lua, 2)?;
    let subject = subject.as_bytes();
    let pattern = pattern.as_bytes();
    let init = initial(args.integer(lua, 3, 1)?, subject.len());
    let none = || MultiValue::from_vec(vec![Value::Nil]);
    if init > subject.len() {
        return Ok(none());
    }
    let mut budget = Budget(MAX_PATTERN_STEPS);
    let plain = args
        .values
        .get(3)
        .is_some_and(|v| !matches!(v, Value::Nil | Value::Boolean(false)));
    if is_find && (plain || !pattern.iter().any(|b| b"^$*+?.([%-".contains(b))) {
        if pattern.len() > subject.len() - init {
            return Ok(none());
        }
        for start in init..=subject.len() - pattern.len() {
            budget.step()?;
            let mut equal = true;
            for i in 0..pattern.len() {
                budget.step()?;
                if subject[start + i] != pattern[i] {
                    equal = false;
                    break;
                }
            }
            if equal {
                return Ok(MultiValue::from_vec(vec![
                    Value::Integer((start + 1) as i64),
                    Value::Integer((start + pattern.len()) as i64),
                ]));
            }
        }
        return Ok(none());
    }
    let anchor = pattern.first() == Some(&b'^');
    let mut matcher = Matcher::new(&subject, &pattern, &mut budget);
    for start in init..=subject.len() {
        matcher.level = 0;
        if let Some(end) = matcher.matches(start, usize::from(anchor), 0)? {
            let mut values = matcher.values(lua, start, end, !is_find)?;
            if is_find {
                values.push_front(Value::Integer(end as i64));
                values.push_front(Value::Integer((start + 1) as i64));
            }
            return Ok(values);
        }
        if anchor {
            break;
        }
    }
    Ok(none())
}

fn gmatch(lua: &Lua, args: MultiValue) -> Result<MultiValue> {
    let args = Arguments { values: args };
    let subject = args.string(lua, 1)?;
    let pattern = args.string(lua, 2)?;
    let mut start = initial(args.integer(lua, 3, 1)?, subject.as_bytes().len())
        .min(subject.as_bytes().len() + 1);
    let mut lastmatch = None;
    let mut budget = Budget(MAX_PATTERN_STEPS);
    let iterator = lua.create_function_mut(move |lua, _: MultiValue| {
        let result = (|| {
            let subject = subject.as_bytes();
            let pattern = pattern.as_bytes();
            let mut matcher = Matcher::new(&subject, &pattern, &mut budget);
            let scan_start = start;
            for src in scan_start..=subject.len() {
                matcher.level = 0;
                if let Some(end) = matcher.matches(src, 0, 0)?
                    && Some(end) != lastmatch
                {
                    start = end;
                    lastmatch = Some(end);
                    return matcher.values(lua, src, end, true);
                }
            }
            Ok(MultiValue::new())
        })();
        response(lua, result.map(Outcome::Values))
    })?;
    Ok(MultiValue::from_vec(vec![Value::Function(wrap(
        lua,
        iterator,
        Api::Iterator,
    )?)]))
}

struct Output<'a> {
    lua: &'a Lua,
    chunks: mlua::Table,
    count: usize,
}

impl Output<'_> {
    fn append(&mut self, bytes: &[u8]) -> Result<()> {
        if !bytes.is_empty() {
            self.count += 1;
            self.chunks
                .raw_set(self.count, self.lua.create_string(bytes)?)?;
        }
        Ok(())
    }
}

fn add_string(
    lua: &Lua,
    matcher: &Matcher<'_>,
    output: &mut Output<'_>,
    replacement: &mlua::LuaString,
    start: usize,
    end: usize,
) -> Result<()> {
    let replacement = replacement.as_bytes();
    let mut offset = 0;
    while offset < replacement.len() {
        let next = replacement[offset..]
            .iter()
            .position(|&c| c == b'%')
            .map_or(replacement.len(), |i| offset + i);
        output.append(&replacement[offset..next])?;
        if next == replacement.len() {
            break;
        }
        match replacement.get(next + 1).copied() {
            Some(b'%') => output.append(b"%")?,
            Some(b'0') => output.append(&matcher.subject[start..end])?,
            Some(digit @ b'1'..=b'9') => {
                let value = matcher.capture(lua, (digit - b'1') as usize, start, end)?;
                output.append(
                    &lua.coerce_string(value)?
                        .expect("capture is a string or position")
                        .as_bytes(),
                )?;
            }
            _ => return Err(error("invalid use of '%' in replacement string")),
        }
        offset = next + 2;
    }
    Ok(())
}

fn gsub(
    lua: &Lua,
    args: MultiValue,
    protected: &Function,
    index: &Function,
    concat: &Function,
) -> Result<Outcome> {
    let args = Arguments { values: args };
    let subject = args.string(lua, 1)?;
    let pattern = args.string(lua, 2)?;
    let bytes = subject.as_bytes();
    let pattern = pattern.as_bytes();
    let max = args.integer(lua, 4, bytes.len() as i64 + 1)?;
    let replacement = args.values.get(2).cloned().unwrap_or(Value::Nil);
    if !matches!(
        replacement,
        Value::String(_)
            | Value::Integer(_)
            | Value::Number(_)
            | Value::Function(_)
            | Value::Table(_)
    ) {
        return Err(error("invalid replacement type"));
    }
    let replacement_string = if matches!(
        replacement,
        Value::String(_) | Value::Integer(_) | Value::Number(_)
    ) {
        lua.coerce_string(replacement.clone())?
    } else {
        None
    };
    let anchor = pattern.first() == Some(&b'^');
    let mut budget = Budget(MAX_PATTERN_STEPS);
    let mut matcher = Matcher::new(&bytes, &pattern, &mut budget);
    // Keep intermediate replacements in Lua's allocator, including nested gsub.
    let mut output = Output {
        lua,
        chunks: lua.create_table()?,
        count: 0,
    };
    let mut src = 0;
    let mut lastmatch = None;
    let mut count = 0;
    let mut changed = false;
    while count < max {
        matcher.level = 0;
        let end = matcher.matches(src, usize::from(anchor), 0)?;
        if let Some(end) = end.filter(|&end| Some(end) != lastmatch) {
            count += 1;
            if let Some(replacement) = &replacement_string {
                add_string(lua, &matcher, &mut output, replacement, src, end)?;
                changed = true;
            } else {
                let call_args = match &replacement {
                    Value::Function(function) => {
                        let mut values = matcher.values(lua, src, end, true)?;
                        values.push_front(Value::Function(function.clone()));
                        values
                    }
                    Value::Table(table) => MultiValue::from_vec(vec![
                        Value::Function(index.clone()),
                        Value::Table(table.clone()),
                        matcher.capture(lua, 0, src, end)?,
                    ]),
                    _ => unreachable!(),
                };
                let (success, value): (bool, Value) = protected.call(call_args)?;
                if !success {
                    return Ok(Outcome::Raised(value));
                }
                match value {
                    Value::Nil | Value::Boolean(false) => output.append(&bytes[src..end])?,
                    Value::String(_) | Value::Integer(_) | Value::Number(_) => {
                        output.append(
                            &lua.coerce_string(value)?
                                .expect("replacement is string or number")
                                .as_bytes(),
                        )?;
                        changed = true;
                    }
                    value => {
                        return Err(error(format!(
                            "invalid replacement value (a {})",
                            lua_type(&value)
                        )));
                    }
                }
            }
            src = end;
            lastmatch = Some(end);
        } else if src < bytes.len() {
            output.append(&bytes[src..src + 1])?;
            src += 1;
        } else {
            break;
        }
        if anchor {
            break;
        }
    }
    let result = if changed {
        output.append(&bytes[src..])?;
        concat.call::<mlua::LuaString>(output.chunks)?
    } else {
        subject
    };
    Ok(Outcome::Values(MultiValue::from_vec(vec![
        Value::String(result),
        Value::Integer(count),
    ])))
}

#[derive(Clone, Copy)]
enum Api {
    Iterator,
    Search,
    Substitute,
}

unsafe extern "C-unwind" {
    fn luaL_typeerror(
        state: *mut ffi::lua_State,
        arg: std::ffi::c_int,
        expected: *const std::ffi::c_char,
    ) -> std::ffi::c_int;
}

unsafe extern "C-unwind" fn facade(state: *mut ffi::lua_State) -> std::ffi::c_int {
    // SAFETY: wrap installs a worker in upvalue 1 returning (success, ...),
    // and an Api discriminant in upvalue 2. Native argument checks preserve
    // Lua's method/alias diagnostics, including tail calls and type __name.
    // Only primitive locals live across Lua calls: lua_error may longjmp, but
    // the worker has returned and dropped every Rust-owned matcher value.
    unsafe {
        let api = ffi::lua_tointeger(state, ffi::lua_upvalueindex(2));
        if api != Api::Iterator as i64 {
            let mut len = 0;
            ffi::luaL_checklstring(state, 1, &mut len);
            ffi::luaL_checklstring(state, 2, std::ptr::null_mut());
            if api == Api::Substitute as i64 {
                ffi::luaL_optinteger(state, 4, len as i64 + 1);
                if !matches!(
                    ffi::lua_type(state, 3),
                    ffi::LUA_TNUMBER | ffi::LUA_TSTRING | ffi::LUA_TFUNCTION | ffi::LUA_TTABLE
                ) {
                    return luaL_typeerror(state, 3, c"string/function/table".as_ptr());
                }
            } else {
                ffi::luaL_optinteger(state, 3, 1);
            }
        }
        let args = ffi::lua_gettop(state);
        ffi::luaL_checkstack(state, 1, c"pattern call".as_ptr());
        ffi::lua_pushvalue(state, ffi::lua_upvalueindex(1));
        ffi::lua_insert(state, 1);
        ffi::lua_call(state, args, ffi::LUA_MULTRET);
        let success = ffi::lua_toboolean(state, 1) != 0;
        ffi::lua_remove(state, 1);
        if !success {
            ffi::lua_error(state);
        }
        ffi::lua_gettop(state)
    }
}

fn wrap(lua: &Lua, worker: Function, api: Api) -> Result<Function> {
    // SAFETY: exec_raw protects allocation errors and provides a locked stack
    // containing worker and api, consumed as facade's two upvalues. A C facade
    // preserves method/alias names even when Lua tail-calls it.
    unsafe {
        lua.exec_raw((worker, api as i64), |state| {
            ffi::lua_pushcclosure(state, facade, 2)
        })
    }
}

pub(super) fn install(lua: &Lua) -> Result<()> {
    let strings: mlua::Table = lua.globals().get("string")?;
    for (name, is_find) in [("find", true), ("match", false)] {
        let function = lua.create_function(move |lua, args: MultiValue| {
            response(lua, find(lua, args, is_find).map(Outcome::Values))
        })?;
        strings.set(name, wrap(lua, function, Api::Search)?)?;
    }
    let function = lua.create_function(move |lua, args: MultiValue| {
        response(lua, gmatch(lua, args).map(Outcome::Values))
    })?;
    strings.set("gmatch", wrap(lua, function, Api::Search)?)?;
    let protected: Function = lua.globals().get("pcall")?;
    let index: Function = lua.load("return function(t, k) return t[k] end").eval()?;
    let concat: Function = lua.globals().get::<mlua::Table>("table")?.get("concat")?;
    let function = lua.create_function(move |lua, args: MultiValue| {
        response(lua, gsub(lua, args, &protected, &index, &concat))
    })?;
    strings.set("gsub", wrap(lua, function, Api::Substitute)?)?;
    // Lua's string metatable __index is this same table, covering s:match(p).
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const DRIVER: &str = r#"
        return function(op, s, p, init, plain, replacement, n)
            local function encode(...)
                local values, out = table.pack(...), {}
                for i = 1, values.n do
                    local v = values[i]
                    local kind = type(v)
                    if kind == 'number' then kind = math.type(v) end
                    out[#out + 1] = kind .. ':' .. tostring(v)
                end
                return table.concat(out, '|')
            end
            local function run()
                if op == 'call' then
                    return encode(replacement())
                elseif op == 'gmatch' then
                    local it, out = string.gmatch(s, p, init), {}
                    for i = 1, 64 do
                        local values = table.pack(it())
                        if values.n == 0 then return table.concat(out, ';') end
                        out[#out + 1] = encode(table.unpack(values, 1, values.n))
                    end
                    error('iterator did not finish')
                elseif op == 'gsub' then
                    return encode(string.gsub(s, p, replacement, n))
                elseif op == 'find' then
                    return encode(string.find(s, p, init, plain))
                else
                    return encode(string.match(s, p, init))
                end
            end
            local ok, result = pcall(run)
            if ok then return 'ok:' .. result end
            local message = tostring(result)
            local colon = string.find(message, ': ', 1, true)
            if string.sub(message, 1, 1) == '[' and colon then
                message = string.sub(message, colon + 2)
            end
            return 'error:' .. message
        end
    "#;

    #[allow(clippy::too_many_arguments)]
    fn compare(
        native: &Lua,
        bounded: &Lua,
        op: &str,
        s: &[u8],
        p: &[u8],
        init: i64,
        plain: bool,
        replacement: &str,
        n: Option<i64>,
    ) {
        let run = |lua: &Lua| -> Vec<u8> {
            let driver: Function = lua.named_registry_value("driver").unwrap();
            driver
                .call::<mlua::LuaString>((
                    op,
                    lua.create_string(s).unwrap(),
                    lua.create_string(p).unwrap(),
                    init,
                    plain,
                    lua.load(replacement)
                        .set_name("replacement")
                        .eval::<Value>()
                        .unwrap(),
                    n,
                ))
                .unwrap()
                .as_bytes()
                .to_vec()
        };
        assert_eq!(
            run(bounded),
            run(native),
            "{op}: subject {s:?}, pattern {p:?}, init {init}, plain {plain}, replacement {replacement}, n {n:?}"
        );
    }

    #[test]
    fn differential_lua54_patterns() {
        let native = Lua::new();
        let bounded = Lua::new();
        bounded.set_memory_limit(256 << 20).unwrap();
        install(&bounded).unwrap();
        for lua in [&native, &bounded] {
            lua.set_named_registry_value(
                "driver",
                lua.load(DRIVER)
                    .set_name("pattern differential")
                    .eval::<Function>()
                    .unwrap(),
            )
            .unwrap();
        }
        let patterns: &[&[u8]] = &[
            b"",
            b"a",
            b".",
            b"^",
            b"$",
            b"^a",
            b"a^",
            b"$a",
            b"^a$",
            b"a*",
            b"a+",
            b"a-",
            b"a?",
            b"a*a",
            b"a-a",
            b"a?a",
            b".*b",
            b".-b",
            b"()",
            b"(a*)()",
            b"((a)b)",
            b"(a*)%1",
            b"()%1",
            b"%0",
            b"%1",
            b"%9",
            b"(a%1)",
            b"(",
            b")",
            b"(a",
            b"(a))",
            b"%",
            b"[",
            b"[^",
            b"[]",
            b"[]]",
            b"[^]]",
            b"[a-z]",
            b"[z-a]",
            b"[%-a]",
            b"[%]]",
            b"[a%]]",
            b"[%a%d]",
            b"[^%s]",
            b"[-a]",
            b"[a-]",
            b"%b()",
            b"%bxx",
            b"%b",
            b"%b(",
            b"%f[%a]%a+",
            b"%f[%z]",
            b"%f[^%a]",
            b"%f",
            b"%fa",
            b"%z",
            b"%Z",
            b"a\0b",
            b"[\0-\xff]",
            b"[%z]",
            b"%q",
            b"%Q",
            b"%*",
            b"%%",
            b"%[",
            b"*",
            b"+",
            b"-",
            b"?",
            b"a**",
            b"a++",
            b"a--",
            b"a??",
        ];
        let subjects: &[&[u8]] = &[
            b"",
            b"a",
            b"aaab",
            b"abab",
            b" (a(b)c) x ",
            b"xxyxx",
            b"ab\0cd",
            b"\xff\x80\0\n9_a]-%",
            b"^a$*+?-[]",
        ];
        let replacements = [
            "return '%0:%1:%%'",
            "return '%2'",
            "return '%'",
            "return '%x'",
            "return 42",
            "return {}",
            "return function(...) return select('#', ...) end",
            "return function() return false end",
            "return function() return nil end",
            "return function() return true end",
            "return setmetatable({}, {__index = function(t, k) return tostring(k) end})",
        ];
        for &p in patterns {
            for &s in subjects {
                for op in ["find", "match", "gmatch", "gsub"] {
                    for init in [i64::MIN, -3, 0, 1, 2, s.len() as i64 + 1, i64::MAX] {
                        compare(&native, &bounded, op, s, p, init, false, "return 'x'", None);
                    }
                }
                compare(&native, &bounded, "find", s, p, 1, true, "return ''", None);
                for replacement in replacements {
                    compare(&native, &bounded, "gsub", s, p, 1, false, replacement, None);
                }
                for n in [-1, 0, 1, 2] {
                    compare(
                        &native,
                        &bounded,
                        "gsub",
                        s,
                        p,
                        1,
                        false,
                        "return '-'",
                        Some(n),
                    );
                }
            }
        }
        let bytes: Vec<u8> = (0..=255).collect();
        for cl in b"acdg lpsuwxzACDGLPSUWXZ".iter().filter(|&&c| c != b' ') {
            compare(
                &native,
                &bounded,
                "gsub",
                &bytes,
                &[b'%', *cl],
                1,
                false,
                "return '!'",
                None,
            );
        }
        for count in [31, 32, 33] {
            compare(
                &native,
                &bounded,
                "match",
                b"",
                &b"()".repeat(count),
                1,
                false,
                "return ''",
                None,
            );
        }
        for count in [198, 199, 200, 201] {
            let mut pattern = b"a?".repeat(count);
            pattern.push(b'b');
            let mut subject = vec![b'a'; count];
            subject.push(b'b');
            compare(
                &native,
                &bounded,
                "match",
                &subject,
                &pattern,
                1,
                false,
                "return ''",
                None,
            );
        }
        for body in [
            "return string.find()",
            "return string.find(nil, 'a')",
            "return string.match(false, 'a')",
            "return string.find('a')",
            "return string.match('a', {})",
            "local v = ('a'):match({}); return v",
            "return ('a'):match({})",
            "local f = string.find; local v = f('a', {}); return v",
            "local f = string.find; return f('a', {})",
            "local t = {f = string.find}; local v = t:f('a'); return v",
            "local t = {f = string.find}; return t:f('a')",
            "return string.find(setmetatable({}, {__name='widget'}), 'a')",
            "local ok, e = pcall(string.find, 'a', {}); return e",
            "return string.find('a', 'a', 1.5)",
            "return string.find('a', 'a', '1.5')",
            "return string.find('a', 'a', {})",
            "return string.match('a', 'a', 0/0)",
            "return string.match('a', 'a', math.huge)",
            "return string.find(12345, 23, '0x1')",
            "return string.find('a.b', '.', nil, 0)",
            "return string.gmatch('abc', '.', nil)()",
            "return string.gsub('a', '.', nil)",
            "return string.gsub('a', '.', true)",
            "return string.gsub('a', '.', 'b', 1.5)",
            "return string.gsub('a', '.', 'b', {})",
            "return string.gsub(123, '%d', 9)",
            "return string.gsub('ab', '()(.)', '%1:%2')",
            "return string.gsub('ab', '()(.)', {[1]='x', [2]='y'})",
            "return string.gsub('ab', '(.)', {a=false, b='z'})",
            "return string.gsub('ab', '(.)', function() return 'x', 'y' end)",
            "return string.gsub('ab', '(.)', function(s) return string.gsub(s, '.', 'q') end)",
            "local e = {}; local ok, got = pcall(string.gsub, 'a', '.', function() error(e) end); return not ok and got == e",
            "local e = {}; local t = setmetatable({}, {__index=function() error(e) end}); local ok, got = pcall(string.gsub, 'a', '.', t); return not ok and got == e",
            "local it = string.gmatch('ab', 'a*'); return it(), it(), it(), it(), it()",
        ] {
            compare(
                &native,
                &bounded,
                "call",
                b"",
                b"",
                1,
                false,
                &format!("return function() {body} end"),
                None,
            );
        }
        let atoms: &[&[u8]] = &[
            b"a", b"b", b".", b"%a", b"%d", b"%s", b"[a-c]", b"[^b]", b"%z", b"%b()", b"%f[%w]",
            b"()", b"(a?)", b"%1", b"%", b"[",
        ];
        let mut seed = 0x4b65_7474_6c65_5434u64;
        let mut next = || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed as usize
        };
        for _ in 0..4000 {
            let mut subject = Vec::new();
            for _ in 0..next() % 12 {
                subject.push(b"abc 09()\0\xff"[next() % 10]);
            }
            let mut pattern = Vec::new();
            if next() % 5 == 0 {
                pattern.push(b'^');
            }
            for _ in 0..next() % 5 {
                pattern.extend_from_slice(atoms[next() % atoms.len()]);
                let suffix = b"*+-? "[next() % 5];
                if suffix != b' ' {
                    pattern.push(suffix);
                }
            }
            if next() % 5 == 0 {
                pattern.push(b'$');
            }
            for op in ["find", "match", "gmatch", "gsub"] {
                compare(
                    &native,
                    &bounded,
                    op,
                    &subject,
                    &pattern,
                    (next() % 25) as i64 - 12,
                    false,
                    replacements[next() % replacements.len()],
                    None,
                );
            }
        }
    }
}
