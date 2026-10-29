// SPDX-License-Identifier: MIT
// Copyright (c) 2026 Tom F. (https://github.com/tomtom215/duckdb-behavioral)

//! Safe views over the raw handles `DuckDB` passes to an aggregate callback.
//!
//! Every aggregate callback receives raw pointers: a data chunk, an array of
//! state pointers, a result vector. Reading them needs `unsafe`, and the
//! callbacks used to wrap whole function bodies in one `unsafe` block. This
//! module is the other way round: each raw handle becomes a small type whose
//! single `unsafe` constructor states what the callback guarantees, and whose
//! methods are safe because they check the one thing the constructor cannot:
//! the row index. The callbacks in the sibling modules are then ordinary safe
//! Rust, with one `unsafe` call per handle.
//!
//! # What `DuckDB` guarantees
//!
//! These hold for every aggregate registered through the C API (checked
//! against `aggregate_function-c.cpp` in `DuckDB` 2.0.0, and by the constant,
//! dictionary and window-frame probes in `tests/extension_load.rs`):
//!
//! - **Inputs are flat.** `CAPIAggregateUpdate` flattens the input chunk, so a
//!   `VARCHAR` literal argument arrives as one string per row, not as a
//!   constant vector. This is load-bearing: in `DuckDB` 2.0.0
//!   `duckdb_vector_get_data` throws a C++ exception on a non-flat vector,
//!   and an exception through these `extern "C"` frames would abort the
//!   process.
//! - **Argument types are the declared ones.** The binder casts each argument
//!   to the parameter type of the overload that was registered, so a column
//!   registered as `BOOLEAN` holds `BOOLEAN` data.
//! - **State pointers are initialised.** Every pointer in a state array was
//!   passed through [`quack_rs::aggregate::FfiState`]'s init callback, and is
//!   destroyed only after the last callback that uses it.
//! - **Combine never merges a state into itself.** [`States::pair`] still
//!   checks this at run time, because `&T` and `&mut T` to one state would be
//!   undefined behaviour.
//!
//! The exception is the known `DuckDB` defect duckdb/duckdb#26109 (see
//! `CLAUDE.md`): three query shapes on `DuckDB` 1.5.6 and earlier pass a state
//! array shorter than the row count. No callback can detect that, and no view
//! can help.

use libduckdb_sys::{
    duckdb_aggregate_state, duckdb_data_chunk, duckdb_data_chunk_get_column_count,
    duckdb_data_chunk_get_size, duckdb_data_chunk_get_vector, duckdb_function_info, duckdb_vector,
    duckdb_vector_get_data, idx_t,
};
use quack_rs::aggregate::{AggregateFunctionInfo, AggregateState, FfiState};
use quack_rs::interval::DuckInterval;
use quack_rs::vector::complex::ListVector;
use quack_rs::vector::{VectorReader, VectorWriter};
use std::marker::PhantomData;

/// Converts a `DuckDB` count to `usize`. Chunk sizes and state counts are at
/// most a few thousand, so a value that does not fit is a corrupted call.
fn to_usize(n: idx_t) -> usize {
    usize::try_from(n).expect("DuckDB passed a count that does not fit in usize")
}

// ── Input chunk ──────────────────────────────────────────────────────────

/// The input chunk of an aggregate `update` callback.
pub struct Chunk {
    raw: duckdb_data_chunk,
    rows: usize,
    cols: usize,
}

impl Chunk {
    /// Wraps the input chunk of the running `update` callback.
    ///
    /// # Safety
    ///
    /// `raw` must be the chunk `DuckDB` passed to the callback that is
    /// running, and neither the `Chunk` nor any column taken from it may be
    /// used after that callback returns.
    pub unsafe fn from_raw(raw: duckdb_data_chunk) -> Self {
        // SAFETY: `raw` is a live chunk (this function's contract).
        let rows = unsafe { duckdb_data_chunk_get_size(raw) };
        // SAFETY: as above.
        let cols = unsafe { duckdb_data_chunk_get_column_count(raw) };
        Self {
            raw,
            rows: to_usize(rows),
            cols: to_usize(cols),
        }
    }

    /// Number of rows in the chunk.
    pub const fn rows(&self) -> usize {
        self.rows
    }

    /// Number of columns in the chunk.
    pub const fn cols(&self) -> usize {
        self.cols
    }

    fn check_col(&self, col: usize) {
        assert!(
            col < self.cols,
            "column {col} out of range for a chunk of {} columns",
            self.cols
        );
    }

    fn vector(&self, col: usize) -> duckdb_vector {
        self.check_col(col);
        // SAFETY: the chunk is live (`from_raw`'s contract) and `col` is in
        // range (checked above).
        unsafe { duckdb_data_chunk_get_vector(self.raw, col as idx_t) }
    }

    fn reader(&self, col: usize) -> VectorReader {
        self.check_col(col);
        // SAFETY: the chunk is live (`from_raw`'s contract) and `col` is in
        // range (checked above).
        unsafe { VectorReader::new(self.raw, col) }
    }

    /// Column `col` as `BOOLEAN`.
    ///
    /// # Safety
    ///
    /// Column `col` of this chunk must be a `BOOLEAN` column. `DuckDB` casts
    /// arguments to the registered parameter types, so this holds for the
    /// columns an overload declares as `BOOLEAN`.
    pub unsafe fn bools(&self, col: usize) -> Bools {
        Bools(self.reader(col))
    }

    /// Column `col` as `TIMESTAMP` or `TIMESTAMPTZ` (both int64 microseconds
    /// since the Unix epoch).
    ///
    /// # Safety
    ///
    /// Column `col` of this chunk must be a `TIMESTAMP` or `TIMESTAMP WITH
    /// TIME ZONE` column.
    pub unsafe fn timestamps(&self, col: usize) -> Timestamps {
        Timestamps(self.reader(col))
    }

    /// Column `col` as `INTERVAL`.
    ///
    /// # Safety
    ///
    /// Column `col` of this chunk must be an `INTERVAL` column.
    pub unsafe fn intervals(&self, col: usize) -> Intervals {
        Intervals(self.reader(col))
    }

    /// Column `col` as `VARCHAR`.
    ///
    /// # Safety
    ///
    /// Column `col` of this chunk must be a `VARCHAR` column.
    pub unsafe fn varchars(&self, col: usize) -> Varchars {
        let reader = self.reader(col);
        let vector = self.vector(col);
        // SAFETY: `vector` is a live `VARCHAR` vector (this function's
        // contract); its data buffer holds one 16-byte `duckdb_string_t` per
        // row.
        let slots = unsafe { duckdb_vector_get_data(vector) }.cast::<[u8; 16]>();
        Varchars { reader, slots }
    }

    /// Columns `range` as `BOOLEAN` (the trailing condition arguments).
    ///
    /// # Safety
    ///
    /// Every column in `range` must be a `BOOLEAN` column.
    pub unsafe fn bool_range(&self, range: std::ops::Range<usize>) -> Vec<Bools> {
        // SAFETY: forwarded from this function's contract.
        range.map(|col| unsafe { self.bools(col) }).collect()
    }
}

/// The failed-bounds-check path, kept out of line so the check itself stays a
/// compare and a rarely taken branch in the per-row loops.
#[cold]
#[inline(never)]
fn out_of_range(i: usize, rows: usize) -> ! {
    // A failed check is a bug in the calling loop, never user input, so it
    // panics (the callback macros turn a panic into a SQL error).
    panic!("row {i} out of range for a vector of {rows} rows")
}

#[inline]
fn check(reader: &VectorReader, i: usize) {
    if !reader.contains(i) {
        out_of_range(i, reader.row_count());
    }
}

#[inline]
fn is_valid(reader: &VectorReader, i: usize) -> bool {
    check(reader, i);
    // SAFETY: `i < row_count` (checked above).
    unsafe { reader.is_valid(i) }
}

/// A `BOOLEAN` input column.
pub struct Bools(VectorReader);

impl Bools {
    /// The value at row `i`, or `None` for `NULL`.
    #[inline]
    pub fn get(&self, i: usize) -> Option<bool> {
        if !is_valid(&self.0, i) {
            return None;
        }
        // SAFETY: `i` is in range and row `i` is non-NULL (checked above);
        // the column is `BOOLEAN` (constructor contract).
        Some(unsafe { self.0.read_bool(i) })
    }

    /// Whether row `i` is `TRUE`; `NULL` counts as `FALSE`.
    #[inline]
    pub fn is_true(&self, i: usize) -> bool {
        self.get(i) == Some(true)
    }
}

/// A `TIMESTAMP` or `TIMESTAMPTZ` input column.
pub struct Timestamps(VectorReader);

impl Timestamps {
    /// Whether row `i` is non-`NULL`.
    #[inline]
    pub fn is_valid(&self, i: usize) -> bool {
        is_valid(&self.0, i)
    }

    /// Microseconds since the Unix epoch at row `i`, or `None` for `NULL`.
    #[inline]
    pub fn get(&self, i: usize) -> Option<i64> {
        if !is_valid(&self.0, i) {
            return None;
        }
        // SAFETY: `i` is in range and row `i` is non-NULL (checked above);
        // the column holds int64 microseconds (constructor contract).
        Some(unsafe { self.0.read_i64(i) })
    }
}

/// An `INTERVAL` input column.
pub struct Intervals(VectorReader);

impl Intervals {
    /// Whether row `i` is non-`NULL`.
    #[inline]
    pub fn is_valid(&self, i: usize) -> bool {
        is_valid(&self.0, i)
    }

    /// The interval at row `i`, or `None` for `NULL`.
    #[inline]
    pub fn get(&self, i: usize) -> Option<DuckInterval> {
        if !is_valid(&self.0, i) {
            return None;
        }
        // SAFETY: `i` is in range and row `i` is non-NULL (checked above);
        // the column is `INTERVAL` (constructor contract).
        Some(unsafe { self.0.read_interval(i) })
    }
}

/// A `VARCHAR` input column.
pub struct Varchars {
    reader: VectorReader,
    /// The column's `duckdb_string_t` array, for [`Varchars::slot`].
    slots: *const [u8; 16],
}

impl Varchars {
    /// The string at row `i`, or `None` for `NULL`.
    #[inline]
    pub fn get(&self, i: usize) -> Option<&str> {
        if !is_valid(&self.reader, i) {
            return None;
        }
        // SAFETY: `i` is in range and row `i` is non-NULL (checked above);
        // the column is `VARCHAR` (constructor contract), and a non-NULL
        // string's bytes live as long as the chunk, which outlives `&self`.
        Some(unsafe { self.reader.read_str(i) })
    }

    /// The raw 16-byte `duckdb_string_t` slot of row `i`, or `None` for `NULL`:
    /// the length plus either the whole string (at most 12 bytes) or a prefix
    /// and a pointer.
    ///
    /// Within one chunk, equal slots imply equal strings, because equal
    /// slots have equal lengths and equal bytes or point at the same bytes.
    /// Unequal slots prove nothing, so callers compare the strings next.
    /// Slots must not be compared across chunks: a pointer can be reused.
    /// A `NULL` row's slot may hold stale or uninitialised bytes, so it is
    /// never read.
    #[inline]
    pub fn slot(&self, i: usize) -> Option<[u8; 16]> {
        if !is_valid(&self.reader, i) {
            return None;
        }
        let slot = self.slots.wrapping_add(i);
        // SAFETY: `slots` is the data buffer of a live `VARCHAR` vector with
        // one 16-byte slot per row, `i < row_count` and row `i` is non-NULL
        // (checked above), so `wrapping_add` stayed inside the buffer and the
        // slot is initialised; `[u8; 16]` has alignment 1.
        Some(unsafe { *slot })
    }
}

/// Packs the conditions that are true at `row` into a bitmask: bit `c` is
/// condition `c`, and `NULL` counts as false.
///
/// The registered overloads pass at most 32 conditions (the width of the
/// mask, `common::event::MAX_EVENT_CONDITIONS`); more would shift past the
/// mask and, in a release build, wrap silently, so tests (debug builds)
/// assert it.
#[inline]
pub fn condition_mask(conditions: &[Bools], row: usize) -> u32 {
    debug_assert!(
        conditions.len() <= u32::BITS as usize,
        "{} conditions do not fit a 32-bit mask",
        conditions.len()
    );
    let mut mask = 0;
    for (bit, condition) in conditions.iter().enumerate() {
        if condition.is_true(row) {
            mask |= 1 << bit;
        }
    }
    mask
}

// ── Aggregate states ─────────────────────────────────────────────────────

/// An array of aggregate state pointers, one per row (update, finalize) or
/// one per merge (combine).
pub struct States<T: AggregateState> {
    ptr: *const duckdb_aggregate_state,
    len: usize,
    _state: PhantomData<T>,
}

impl<T: AggregateState> States<T> {
    /// Wraps the state array of the running callback.
    ///
    /// # Safety
    ///
    /// `ptr` must point to `len` non-null state pointers that `DuckDB` passed
    /// to the callback that is running, each created by `FfiState::<T>`'s init
    /// callback (every overload registers it through `ffi_state::<T>()`), and
    /// the `States` must not be used after the callback returns.
    pub unsafe fn from_raw(ptr: *mut duckdb_aggregate_state, len: idx_t) -> Self {
        Self {
            ptr: ptr.cast_const(),
            len: to_usize(len),
            _state: PhantomData,
        }
    }

    /// Number of state pointers.
    pub const fn count(&self) -> usize {
        self.len
    }

    /// The pointer at index `i`. Two indices holding equal handles are the
    /// same state.
    #[inline]
    fn handle(&self, i: usize) -> duckdb_aggregate_state {
        assert!(i < self.len, "state {i} out of range of {}", self.len);
        let slot = self.ptr.wrapping_add(i);
        // SAFETY: `ptr` points to `len` pointers (constructor contract) and
        // `i < len` (asserted above), so `wrapping_add` stayed inside the
        // array.
        unsafe { *slot }
    }

    /// The state at index `i`, or `None` if the slot holds no `T` (its
    /// `Default` panicked, and `DuckDB` is about to fail the query).
    #[inline]
    pub fn get(&self, i: usize) -> Option<&T> {
        let handle = self.handle(i);
        // SAFETY: the pointer is an initialised `FfiState<T>` (constructor
        // contract, which includes non-null). The returned borrow is tied to
        // `&self`, and no `&mut` to this array's states exists while it lives.
        unsafe { FfiState::<T>::with_state(handle) }
    }

    /// The state at index `i` as a mutable reference, or `None` if the slot
    /// holds no `T`, as for [`States::get`]. Rows of one group repeat a pointer, so the borrow is
    /// tied to `&mut self`: two references to one state cannot coexist.
    // `&mut self` is the point: it stops two `&mut T` to one state coexisting.
    #[allow(clippy::needless_pass_by_ref_mut)]
    #[inline]
    pub fn get_mut(&mut self, i: usize) -> Option<&mut T> {
        let handle = self.handle(i);
        // SAFETY: the pointer is an initialised `FfiState<T>` (constructor
        // contract, which includes non-null), and `&mut self` makes this the
        // only live reference obtained through this array.
        unsafe { FfiState::<T>::with_state_mut(handle) }
    }

    /// The end of the run of rows that start at `start` and share one state:
    /// `DuckDB` hands consecutive rows of a group the same pointer, so with
    /// one large group the whole chunk is a single run.
    #[inline]
    pub fn run_end(&self, start: usize) -> usize {
        let handle = self.handle(start);
        let mut end = start + 1;
        while end < self.len && self.handle(end) == handle {
            end += 1;
        }
        end
    }

    /// Source `i` and target `i` of a combine: a shared reference to the
    /// source and a mutable one to the target.
    ///
    /// # Panics
    ///
    /// If both pointers are the same state, which `DuckDB` never does and
    /// which would make the two references alias.
    // `&mut Self` for the target ties the returned `&mut T` to a unique borrow.
    #[allow(clippy::needless_pass_by_ref_mut)]
    pub fn pair<'a>(
        source: &'a Self,
        target: &'a mut Self,
        i: usize,
    ) -> Option<(&'a T, &'a mut T)> {
        let (src, tgt) = (source.handle(i), target.handle(i));
        assert!(src != tgt, "DuckDB combined a state into itself");
        // SAFETY: both pointers are initialised `FfiState<T>`s (constructor
        // contract, which includes non-null) and are distinct (asserted above), so the
        // shared and mutable references do not alias. Their lifetimes are
        // tied to the borrows of the two arrays.
        let src = unsafe { FfiState::<T>::with_state(src) }?;
        // SAFETY: as above; `target` is borrowed mutably for `'a`.
        let tgt = unsafe { FfiState::<T>::with_state_mut(tgt) }?;
        Some((src, tgt))
    }
}

// ── Result vectors ───────────────────────────────────────────────────────

/// Marker for a `BOOLEAN` result vector.
pub enum BoolKind {}
/// Marker for an `INTEGER` result vector.
pub enum I32Kind {}
/// Marker for a `BIGINT` result vector.
pub enum I64Kind {}
/// Marker for a `VARCHAR` result vector.
pub enum VarcharKind {}
/// Marker for a `LIST(BOOLEAN)` result vector.
pub enum BoolListKind {}
/// Marker for a `LIST(TIMESTAMP)` or `LIST(TIMESTAMPTZ)` result vector.
pub enum TimestampListKind {}

/// The result vector of a finalize callback, which writes `count` rows
/// starting at `offset`. `K` is the vector's type, so only writes that fit it
/// compile.
pub struct Output<K> {
    raw: duckdb_vector,
    writer: VectorWriter,
    offset: usize,
    count: usize,
    _kind: PhantomData<K>,
}

impl<K> Output<K> {
    /// Wraps the result vector of the running `finalize` callback.
    ///
    /// # Safety
    ///
    /// `raw` must be the result vector `DuckDB` passed to the callback that is
    /// running, with room for `offset + count` rows and of the type that `K`
    /// names (the overload's registered return type), and the `Output` must
    /// not be used after the callback returns.
    pub unsafe fn from_raw(raw: duckdb_vector, count: idx_t, offset: idx_t) -> Self {
        // SAFETY: `raw` is a live result vector (this function's contract).
        let writer = unsafe { VectorWriter::new(raw) };
        Self {
            raw,
            writer,
            offset: to_usize(offset),
            count: to_usize(count),
            _kind: PhantomData,
        }
    }

    /// Number of rows to write.
    pub const fn count(&self) -> usize {
        self.count
    }

    fn idx(&self, i: usize) -> usize {
        assert!(
            i < self.count,
            "result row {i} out of range of {}",
            self.count
        );
        self.offset + i
    }

    /// Sets result row `i` to `NULL`.
    pub fn set_null(&mut self, i: usize) {
        let idx = self.idx(i);
        // SAFETY: `idx < offset + count`, which the vector has room for
        // (constructor contract).
        unsafe { self.writer.set_null(idx) };
    }
}

impl Output<BoolKind> {
    /// Writes `value` to result row `i`.
    pub fn write(&mut self, i: usize, value: bool) {
        let idx = self.idx(i);
        // SAFETY: `idx` is in range and the vector is `BOOLEAN` (constructor
        // contract).
        unsafe { self.writer.write_bool(idx, value) };
    }
}

impl Output<I32Kind> {
    /// Writes `value` to result row `i`.
    pub fn write(&mut self, i: usize, value: i32) {
        let idx = self.idx(i);
        // SAFETY: `idx` is in range and the vector is `INTEGER` (constructor
        // contract).
        unsafe { self.writer.write_i32(idx, value) };
    }
}

impl Output<I64Kind> {
    /// Writes `value` to result row `i`.
    pub fn write(&mut self, i: usize, value: i64) {
        let idx = self.idx(i);
        // SAFETY: `idx` is in range and the vector is `BIGINT` (constructor
        // contract).
        unsafe { self.writer.write_i64(idx, value) };
    }
}

impl Output<VarcharKind> {
    /// Writes `value` to result row `i`. `DuckDB` copies the bytes, so
    /// strings of any length work.
    pub fn write(&mut self, i: usize, value: &str) {
        let idx = self.idx(i);
        // SAFETY: `idx` is in range and the vector is `VARCHAR` (constructor
        // contract); `write_varchar` passes the length, so no NUL terminator
        // is needed.
        unsafe { self.writer.write_varchar(idx, value) };
    }
}

/// The largest list child size this module will ask `DuckDB` to reserve.
/// `duckdb_list_vector_reserve` throws a C++ exception above `DuckDB`'s
/// `MAX_VECTOR_SIZE`, and unwinding that through Rust frames is undefined
/// behaviour; this bound is far below it and far above any real output (at
/// most 1,024 entries per row, 2,048 rows per vector).
const MAX_LIST_CHILD: usize = 1 << 28;

/// Shared by both list kinds: appends `values` as the list of row `i`.
fn push_list<K, V: Copy>(
    out: &mut Output<K>,
    i: usize,
    values: &[V],
    write: impl Fn(&mut VectorWriter, usize, V),
) {
    let idx = out.idx(i);
    // SAFETY: the vector is a live `LIST` vector (constructor contract).
    let start = unsafe { ListVector::get_size(out.raw) };
    let end = start + values.len();
    assert!(
        end <= MAX_LIST_CHILD,
        "list result of {end} child values is too large"
    );
    // SAFETY: as above; `end <= MAX_LIST_CHILD` keeps the reservation below
    // the size at which DuckDB throws.
    unsafe { ListVector::reserve(out.raw, end) };
    // SAFETY: as above.
    let mut child = unsafe { ListVector::child_writer(out.raw) };
    for (j, &value) in values.iter().enumerate() {
        write(&mut child, start + j, value);
    }
    // SAFETY: as above; the child has room for `end` values (reserved above).
    unsafe { ListVector::set_size(out.raw, end) };
    // SAFETY: as above; `idx` is in range and the entry covers values just
    // written.
    unsafe { ListVector::set_entry(out.raw, idx, start as u64, values.len() as u64) };
}

impl Output<TimestampListKind> {
    /// Sets result row `i` to the list of `micros` (microseconds since the
    /// Unix epoch); an empty slice gives an empty list.
    pub fn push(&mut self, i: usize, micros: &[i64]) {
        push_list(self, i, micros, |child, at, value| {
            // SAFETY: `at` is below the reserved child size and the child is
            // a timestamp vector (constructor contract).
            unsafe { child.write_i64(at, value) };
        });
    }
}

impl Output<BoolListKind> {
    /// Sets result row `i` to the list of `values`; an empty slice gives an
    /// empty list.
    pub fn push(&mut self, i: usize, values: &[bool]) {
        push_list(self, i, values, |child, at, value| {
            // SAFETY: `at` is below the reserved child size and the child is
            // a `BOOLEAN` vector (constructor contract).
            unsafe { child.write_bool(at, value) };
        });
    }
}

// ── Whole-callback bundles ───────────────────────────────────────────────

/// Everything an aggregate `update` callback receives.
pub struct Update<T: AggregateState> {
    /// Reports errors back to `DuckDB`.
    pub info: AggregateFunctionInfo,
    /// The input rows.
    pub chunk: Chunk,
    /// One state pointer per input row.
    pub states: States<T>,
}

impl<T: AggregateState> Update<T> {
    /// Wraps the arguments of the running `update` callback.
    ///
    /// # Safety
    ///
    /// The three handles must be the arguments `DuckDB` passed to the
    /// `update` callback that is running, registered with
    /// `FfiState::<T>` as the aggregate state, and the result must not be
    /// used after the callback returns.
    pub unsafe fn from_raw(
        info: duckdb_function_info,
        input: duckdb_data_chunk,
        states: *mut duckdb_aggregate_state,
    ) -> Self {
        // SAFETY: `info` is the running callback's handle (contract).
        let info = unsafe { AggregateFunctionInfo::new(info) };
        // SAFETY: `input` is the running callback's input chunk (contract).
        let chunk = unsafe { Chunk::from_raw(input) };
        // SAFETY: `states` holds one initialised `FfiState<T>` pointer per
        // input row (contract).
        let states = unsafe { States::from_raw(states, chunk.rows() as idx_t) };
        Self {
            info,
            chunk,
            states,
        }
    }
}

/// Everything an aggregate `combine` callback receives.
pub struct Combine<T: AggregateState> {
    /// Reports errors back to `DuckDB`.
    pub info: AggregateFunctionInfo,
    /// The states to merge from.
    pub source: States<T>,
    /// The states to merge into; `source[i]` is merged into `target[i]`.
    pub target: States<T>,
}

impl<T: AggregateState> Combine<T> {
    /// Wraps the arguments of the running `combine` callback.
    ///
    /// # Safety
    ///
    /// The handles must be the arguments `DuckDB` passed to the `combine`
    /// callback that is running (`source` and `target` each holding `count`
    /// initialised `FfiState::<T>` pointers), and the result must not be
    /// used after the callback returns.
    pub unsafe fn from_raw(
        info: duckdb_function_info,
        source: *mut duckdb_aggregate_state,
        target: *mut duckdb_aggregate_state,
        count: idx_t,
    ) -> Self {
        // SAFETY: `info` is the running callback's handle (contract).
        let info = unsafe { AggregateFunctionInfo::new(info) };
        // SAFETY: `source` holds `count` initialised pointers (contract).
        let source = unsafe { States::from_raw(source, count) };
        // SAFETY: `target` holds `count` initialised pointers (contract).
        let target = unsafe { States::from_raw(target, count) };
        Self {
            info,
            source,
            target,
        }
    }
}

/// Everything an aggregate `finalize` callback receives.
pub struct Finalize<T: AggregateState, K> {
    /// Reports errors back to `DuckDB`.
    pub info: AggregateFunctionInfo,
    /// The states to read; `states[i]` produces result row `i`.
    pub states: States<T>,
    /// The result vector.
    pub out: Output<K>,
}

impl<T: AggregateState, K> Finalize<T, K> {
    /// Wraps the arguments of the running `finalize` callback.
    ///
    /// # Safety
    ///
    /// The handles must be the arguments `DuckDB` passed to the `finalize`
    /// callback that is running: `source` holds `count` initialised
    /// `FfiState::<T>` pointers, and `result` is a vector of the type `K`
    /// names with room for `offset + count` rows. The result must not be used
    /// after the callback returns.
    pub unsafe fn from_raw(
        info: duckdb_function_info,
        source: *mut duckdb_aggregate_state,
        result: duckdb_vector,
        count: idx_t,
        offset: idx_t,
    ) -> Self {
        // SAFETY: `info` is the running callback's handle (contract).
        let info = unsafe { AggregateFunctionInfo::new(info) };
        // SAFETY: `source` holds `count` initialised pointers (contract).
        let states = unsafe { States::from_raw(source, count) };
        // SAFETY: `result` is a vector of type `K` with room for
        // `offset + count` rows (contract).
        let out = unsafe { Output::from_raw(result, count, offset) };
        Self { info, states, out }
    }
}
