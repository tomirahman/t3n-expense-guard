//! The audit ledger in `z:<tid>:audit`.
//!
//! Layout (`docs/INTERFACE.md` §"Maps"):
//!
//! * `seq:<000001>` → the JSON audit record (append-only, write-once);
//! * `exp:<expense_id>` → a pointer/index entry for that expense.
//!
//! The **record** shape is frozen. The **pointer** value is not specified by
//! INTERFACE.md, but `possible_duplicate` must compare the original
//! `employee_ref` + `amount` + `currency` + `vendor` (the record carries only
//! the converted `base_amount`), so the pointer stores a small JSON fingerprint
//! index; it also carries the latest `seq` for the expense, which is what
//! `request-approval` uses to inherit the claim's fields into its own record.
//!
//! Sequence numbers are zero-padded to six digits, which makes lexicographic key
//! order equal to numeric order (up to `MAX_SEQ`, see [`highest_seq`]).

use serde::{Deserialize, Serialize};

use crate::capabilities::Host;
use crate::errors;
use crate::policy::round2;

/// Audit record key prefix.
pub const SEQ_PREFIX: &str = "seq:";
/// Expense pointer key prefix.
pub const EXPENSE_PREFIX: &str = "exp:";
/// Exclusive upper bound of the `seq:` range (`':'` is 0x3a, `';'` is 0x3b).
pub const SEQ_END: &[u8] = b"seq;";
/// Exclusive upper bound of the `exp:` range.
pub const EXPENSE_END: &[u8] = b"exp;";
/// Zero-padding width of the sequence number.
pub const SEQ_DIGITS: usize = 6;
/// Records requested per scan page (the host's scan budget).
pub const PAGE_LIMIT: u32 = 1_000;
/// `limit` used by `get-audit` when the caller omits it.
pub const DEFAULT_READ_LIMIT: usize = 20;
/// Hard cap on `get-audit` page size.
pub const MAX_READ_LIMIT: usize = 200;
/// Largest sequence representable with six digits of zero padding.
pub const MAX_SEQ: u64 = 999_999;

/// One write-once audit record. Field order is the frozen wire order.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AuditRecord {
    pub seq: u64,
    pub expense_id: String,
    pub employee_ref: String,
    pub verdict: String,
    pub base_amount: Option<f64>,
    pub category: String,
    pub vendor: String,
    pub rule_hits: Vec<String>,
    pub recorded_at: u64,
    pub contract_version: String,
}

/// `exp:<expense_id>` index entry: where the expense's latest record is, plus
/// the claim fingerprint needed by the duplicate check.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExpensePointer {
    pub seq: u64,
    pub recorded_at: u64,
    pub employee_ref: String,
    pub amount: f64,
    pub currency: String,
    pub vendor: String,
}

/// `get-audit` filter (all fields optional).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct AuditFilter {
    #[serde(default)]
    pub expense_id: Option<String>,
    #[serde(default)]
    pub employee_ref: Option<String>,
    #[serde(default)]
    pub limit: Option<usize>,
}

impl AuditFilter {
    /// Requested page size, clamped to `1..=MAX_READ_LIMIT`.
    pub fn resolved_limit(&self) -> usize {
        self.limit
            .unwrap_or(DEFAULT_READ_LIMIT)
            .clamp(1, MAX_READ_LIMIT)
    }

    /// Does a record satisfy the filter?
    pub fn matches(&self, record: &AuditRecord) -> bool {
        if let Some(expense_id) = &self.expense_id {
            if &record.expense_id != expense_id {
                return false;
            }
        }
        if let Some(employee_ref) = &self.employee_ref {
            if &record.employee_ref != employee_ref {
                return false;
            }
        }
        true
    }
}

/// The claim fields the duplicate rule compares against previous expenses.
#[derive(Debug, Clone, PartialEq)]
pub struct DuplicateCandidate {
    pub employee_ref: String,
    pub amount: f64,
    pub currency: String,
    pub vendor: String,
}

impl DuplicateCandidate {
    /// Same employee, same amount (to the cent), same currency, same vendor.
    fn matches(&self, pointer: &ExpensePointer) -> bool {
        pointer.employee_ref == self.employee_ref
            && round2(pointer.amount) == round2(self.amount)
            && pointer.currency.eq_ignore_ascii_case(&self.currency)
            && pointer.vendor == self.vendor
    }
}

/// `seq:<000001>` — zero-padded so key order equals numeric order.
pub fn seq_key(seq: u64) -> Vec<u8> {
    format!("{SEQ_PREFIX}{seq:0SEQ_DIGITS$}").into_bytes()
}

/// Parse a `seq:` key back into its number. `None` for anything else.
pub fn parse_seq_key(key: &[u8]) -> Option<u64> {
    let text = std::str::from_utf8(key).ok()?;
    let digits = text.strip_prefix(SEQ_PREFIX)?;
    if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    digits.parse().ok()
}

/// `exp:<expense_id>`.
pub fn expense_key(expense_id: &str) -> Vec<u8> {
    format!("{EXPENSE_PREFIX}{expense_id}").into_bytes()
}

/// Is `recorded_at` inside a `days`-long window ending at `now`?
///
/// Inclusive at the boundary (`days = 0` matches only the same second), and a
/// future `recorded_at` is never a duplicate.
pub fn within_window(now: u64, recorded_at: u64, days: u64) -> bool {
    recorded_at <= now && now - recorded_at <= days.saturating_mul(86_400)
}

fn kv_error(context: &str, error: &str) -> String {
    errors::prefixed(errors::KV, &format!("{context}: {error}"))
}

/// Non-empty when at least one key exists at or above `seq`.
fn exists_at_or_above(host: &mut impl Host, map: &str, seq: u64) -> Result<bool, String> {
    let page = host
        .kv_scan(map, &seq_key(seq), SEQ_END, 1)
        .map_err(|e| kv_error("audit scan", &e))?;
    Ok(!page.is_empty())
}

/// Highest sequence number in the ledger (`0` when empty).
///
/// One scan page covers the common case. If the page comes back full the ledger
/// may be longer than one page, so the true maximum is located with a binary
/// probe over the sequence space (`~20` single-entry scans worst case) — the
/// alternative, trusting the page, would silently reuse sequence numbers on a
/// ledger longer than `PAGE_LIMIT`.
pub fn highest_seq(host: &mut impl Host, map: &str) -> Result<u64, String> {
    let page = host
        .kv_scan(map, SEQ_PREFIX.as_bytes(), SEQ_END, PAGE_LIMIT)
        .map_err(|e| kv_error("audit scan", &e))?;
    let page_max = page
        .iter()
        .filter_map(|(key, _)| parse_seq_key(key))
        .max()
        .unwrap_or(0);

    if page.len() < PAGE_LIMIT as usize {
        return Ok(page_max);
    }

    let mut low = page_max;
    let mut high = MAX_SEQ;
    while low < high {
        let mid = low + (high - low).div_ceil(2);
        if exists_at_or_above(host, map, mid)? {
            low = mid;
        } else {
            high = mid - 1;
        }
    }
    Ok(low)
}

/// Next free sequence number.
pub fn next_seq(host: &mut impl Host, map: &str) -> Result<u64, String> {
    let seq = highest_seq(host, map)? + 1;
    if seq > MAX_SEQ {
        return Err(kv_error(
            "audit append",
            "sequence space exhausted (999999 records)",
        ));
    }
    Ok(seq)
}

/// Append a record at `seq`. Returns the sequence number written.
pub fn append_at(
    host: &mut impl Host,
    map: &str,
    seq: u64,
    record: &mut AuditRecord,
) -> Result<u64, String> {
    record.seq = seq;
    let encoded =
        serde_json::to_vec(record).map_err(|e| kv_error("audit encode", &e.to_string()))?;
    host.kv_put(map, &seq_key(seq), &encoded)
        .map_err(|e| kv_error("audit append", &e))?;
    Ok(seq)
}

/// Append a record at the next free sequence number.
pub fn append(host: &mut impl Host, map: &str, record: &mut AuditRecord) -> Result<u64, String> {
    let seq = next_seq(host, map)?;
    append_at(host, map, seq, record)
}

/// Write the `exp:<expense_id>` index entry (`expense_id` is the entry key, so
/// it is passed explicitly rather than duplicated inside the value).
pub fn write_pointer(
    host: &mut impl Host,
    map: &str,
    expense_id: &str,
    pointer: &ExpensePointer,
) -> Result<(), String> {
    let encoded =
        serde_json::to_vec(pointer).map_err(|e| kv_error("pointer encode", &e.to_string()))?;
    host.kv_put(map, &expense_key(expense_id), &encoded)
        .map_err(|e| kv_error("pointer write", &e))
}

/// Refresh an existing pointer to point at a newer record, keeping the claim
/// fingerprint it already carries.
pub fn refresh_pointer(
    host: &mut impl Host,
    map: &str,
    expense_id: &str,
    previous: &ExpensePointer,
    seq: u64,
    recorded_at: u64,
) -> Result<(), String> {
    let updated = ExpensePointer {
        seq,
        recorded_at,
        ..previous.clone()
    };
    write_pointer(host, map, expense_id, &updated)
}

/// Read one record by sequence number.
pub fn read_record(
    host: &mut impl Host,
    map: &str,
    seq: u64,
) -> Result<Option<AuditRecord>, String> {
    let bytes = host
        .kv_get(map, &seq_key(seq))
        .map_err(|e| kv_error("audit read", &e))?;
    Ok(bytes.and_then(|bytes| serde_json::from_slice(&bytes).ok()))
}

/// Read the pointer for one expense.
pub fn pointer_for(
    host: &mut impl Host,
    map: &str,
    expense_id: &str,
) -> Result<Option<ExpensePointer>, String> {
    let bytes = host
        .kv_get(map, &expense_key(expense_id))
        .map_err(|e| kv_error("pointer read", &e))?;
    Ok(bytes.and_then(|bytes| serde_json::from_slice(&bytes).ok()))
}

/// Read a page of records starting at `start_seq`, oldest first.
///
/// Unparseable entries are skipped rather than failing the read: a corrupt
/// record must not make the whole ledger unreadable.
pub fn read_page(
    host: &mut impl Host,
    map: &str,
    start_seq: u64,
    limit: u32,
) -> Result<Vec<AuditRecord>, String> {
    let page = host
        .kv_scan(map, &seq_key(start_seq), SEQ_END, limit)
        .map_err(|e| kv_error("audit scan", &e))?;
    Ok(page
        .into_iter()
        .filter_map(|(key, value)| {
            let mut record: AuditRecord = serde_json::from_slice(&value).ok()?;
            // Trust the key over the payload for ordering purposes.
            if let Some(seq) = parse_seq_key(&key) {
                record.seq = seq;
            }
            Some(record)
        })
        .collect())
}

/// Newest-first page of records matching `filter`.
pub fn read_newest(
    host: &mut impl Host,
    map: &str,
    filter: &AuditFilter,
) -> Result<Vec<AuditRecord>, String> {
    let max_seq = highest_seq(host, map)?;
    if max_seq == 0 {
        return Ok(Vec::new());
    }
    let limit = filter.resolved_limit();
    let start = max_seq.saturating_sub(limit as u64 - 1).max(1);
    let mut records = read_page(host, map, start, limit as u32)?;
    records.retain(|record| filter.matches(record));
    records.reverse();
    Ok(records)
}

/// Is this claim a likely duplicate of an earlier expense?
///
/// Walks the `exp:` pointer set (bounded by `PAGE_LIMIT` entries) and compares
/// employee, amount, currency, vendor and the `duplicate_window_days` window.
pub fn find_duplicate(
    host: &mut impl Host,
    map: &str,
    candidate: &DuplicateCandidate,
    window_days: u64,
    now: u64,
) -> Result<bool, String> {
    let page = host
        .kv_scan(map, EXPENSE_PREFIX.as_bytes(), EXPENSE_END, PAGE_LIMIT)
        .map_err(|e| kv_error("expense index scan", &e))?;
    for (_, value) in page {
        let Ok(pointer) = serde_json::from_slice::<ExpensePointer>(&value) else {
            continue;
        };
        if candidate.matches(&pointer) && within_window(now, pointer.recorded_at, window_days) {
            return Ok(true);
        }
    }
    Ok(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::MockHost;

    const MAP: &str = "z:t:audit";
    const NOW: u64 = 1_700_000_000;
    const DAY: u64 = 86_400;

    fn sample_record(seq: u64, expense_id: &str, employee_ref: &str) -> AuditRecord {
        AuditRecord {
            seq,
            expense_id: expense_id.to_string(),
            employee_ref: employee_ref.to_string(),
            verdict: "needs_approval".to_string(),
            base_amount: Some(884.52),
            category: "travel".to_string(),
            vendor: "Lufthansa".to_string(),
            rule_hits: vec!["over_approval_threshold".to_string()],
            recorded_at: NOW,
            contract_version: "0.1.0".to_string(),
        }
    }

    fn pointer(_expense_id: &str, recorded_at: u64) -> ExpensePointer {
        ExpensePointer {
            seq: 1,
            recorded_at,
            employee_ref: "emp_7f3a".to_string(),
            amount: 812.4,
            currency: "EUR".to_string(),
            vendor: "Lufthansa".to_string(),
        }
    }

    #[test]
    fn seq_key_zero_pads_to_six_digits() {
        assert_eq!(seq_key(1), b"seq:000001".to_vec());
        assert_eq!(seq_key(7), b"seq:000007".to_vec());
        assert_eq!(seq_key(999_999), b"seq:999999".to_vec());
        assert_eq!(seq_key(1).len(), SEQ_PREFIX.len() + SEQ_DIGITS);
    }

    #[test]
    fn seq_key_order_matches_numeric_order() {
        assert!(seq_key(9) < seq_key(10));
        assert!(seq_key(999) < seq_key(1_000));
        assert!(seq_key(MAX_SEQ) < SEQ_END.to_vec());
    }

    #[test]
    fn parse_seq_key_round_trips_and_rejects_other_keys() {
        for seq in [1_u64, 42, 999_999] {
            assert_eq!(parse_seq_key(&seq_key(seq)), Some(seq));
        }
        assert_eq!(parse_seq_key(b"exp:EXP-1"), None);
        assert_eq!(parse_seq_key(b"seq:"), None);
        assert_eq!(parse_seq_key(b"seq:abc"), None);
        assert_eq!(parse_seq_key(b"seq:000001x"), None);
    }

    #[test]
    fn expense_key_is_prefixed_with_the_expense_id() {
        assert_eq!(expense_key("EXP-1042"), b"exp:EXP-1042".to_vec());
    }

    #[test]
    fn duplicate_window_maths_is_inclusive_at_the_boundary() {
        assert!(within_window(NOW, NOW, 30));
        assert!(within_window(NOW, NOW - 30 * DAY, 30));
        assert!(!within_window(NOW, NOW - 30 * DAY - 1, 30));
        assert!(
            !within_window(NOW, NOW + 1, 30),
            "future record is not a duplicate"
        );
        assert!(within_window(NOW, NOW, 0));
        assert!(!within_window(NOW, NOW - 1, 0));
    }

    #[test]
    fn append_assigns_sequence_and_writes_the_record() {
        let mut host = MockHost::new();
        let mut record = sample_record(0, "EXP-1042", "emp_7f3a");
        let seq = append(&mut host, MAP, &mut record).unwrap();
        assert_eq!(seq, 1);
        assert_eq!(
            record.seq, 1,
            "the caller's record is stamped with its sequence"
        );

        let stored: serde_json::Value = host.json_at(MAP, b"seq:000001").unwrap();
        assert_eq!(stored["seq"], 1);
        assert_eq!(stored["verdict"], "needs_approval");
        assert_eq!(stored["contract_version"], "0.1.0");

        let mut second = sample_record(0, "EXP-1043", "emp_7f3a");
        assert_eq!(append(&mut host, MAP, &mut second).unwrap(), 2);
    }

    #[test]
    fn append_at_honours_a_pre_reserved_sequence() {
        let mut host = MockHost::new();
        let mut record = sample_record(0, "EXP-1042", "emp_7f3a");
        assert_eq!(append_at(&mut host, MAP, 4, &mut record).unwrap(), 4);
        assert_eq!(record.seq, 4);
        assert!(host.value_at(MAP, b"seq:000004").is_some());
    }

    #[test]
    fn next_seq_guards_the_sequence_space() {
        let mut host = MockHost::new();
        assert_eq!(next_seq(&mut host, MAP).unwrap(), 1);
        let mut record = sample_record(0, "EXP-1", "emp_1");
        append_at(&mut host, MAP, MAX_SEQ, &mut record).unwrap();
        let error = next_seq(&mut host, MAP).unwrap_err();
        assert!(error.starts_with(errors::KV), "{error}");
        assert!(error.contains("exhausted"));
    }

    #[test]
    fn highest_seq_finds_the_true_maximum_beyond_one_scan_page() {
        let mut host = MockHost::new();
        // PAGE_LIMIT + 200 records: the first page cannot contain the maximum.
        for seq in 1..=PAGE_LIMIT as u64 + 200 {
            let mut record = sample_record(0, &format!("EXP-{seq}"), "emp_7f3a");
            append_at(&mut host, MAP, seq, &mut record).unwrap();
        }
        assert_eq!(
            highest_seq(&mut host, MAP).unwrap(),
            PAGE_LIMIT as u64 + 200
        );
        assert_eq!(next_seq(&mut host, MAP).unwrap(), PAGE_LIMIT as u64 + 201);
    }

    #[test]
    fn highest_seq_is_zero_for_an_empty_ledger() {
        let mut host = MockHost::new();
        assert_eq!(highest_seq(&mut host, MAP).unwrap(), 0);
    }

    #[test]
    fn pointer_round_trip_keeps_the_claim_fingerprint() {
        let mut host = MockHost::new();
        write_pointer(&mut host, MAP, "EXP-1042", &pointer("EXP-1042", NOW)).unwrap();
        let stored = pointer_for(&mut host, MAP, "EXP-1042").unwrap().unwrap();
        assert_eq!(stored, pointer("EXP-1042", NOW));
        assert_eq!(pointer_for(&mut host, MAP, "EXP-9999").unwrap(), None);
    }

    #[test]
    fn refresh_pointer_moves_the_index_forward_without_losing_the_fingerprint() {
        let mut host = MockHost::new();
        let previous = pointer("EXP-1042", NOW);
        write_pointer(&mut host, MAP, "EXP-1042", &previous).unwrap();
        refresh_pointer(&mut host, MAP, "EXP-1042", &previous, 8, NOW + 60).unwrap();

        let stored = pointer_for(&mut host, MAP, "EXP-1042").unwrap().unwrap();
        assert_eq!(stored.seq, 8);
        assert_eq!(stored.recorded_at, NOW + 60);
        assert_eq!(stored.amount, previous.amount);
        assert_eq!(stored.currency, previous.currency);
    }

    #[test]
    fn read_newest_returns_newest_first_and_honours_the_limit() {
        let mut host = MockHost::new();
        for seq in 1..=3 {
            let mut record = sample_record(0, &format!("EXP-{seq}"), "emp_7f3a");
            append_at(&mut host, MAP, seq, &mut record).unwrap();
        }
        let filter = AuditFilter {
            limit: Some(2),
            ..AuditFilter::default()
        };
        let records = read_newest(&mut host, MAP, &filter).unwrap();
        assert_eq!(
            records.iter().map(|r| r.seq).collect::<Vec<_>>(),
            vec![3, 2],
            "newest first"
        );
    }

    #[test]
    fn read_newest_filters_by_expense_and_employee() {
        let mut host = MockHost::new();
        let mut first = sample_record(0, "EXP-1", "emp_a");
        let mut second = sample_record(0, "EXP-2", "emp_b");
        let mut third = sample_record(0, "EXP-3", "emp_a");
        append(&mut host, MAP, &mut first).unwrap();
        append(&mut host, MAP, &mut second).unwrap();
        append(&mut host, MAP, &mut third).unwrap();

        let by_employee = read_newest(
            &mut host,
            MAP,
            &AuditFilter {
                employee_ref: Some("emp_a".to_string()),
                ..AuditFilter::default()
            },
        )
        .unwrap();
        assert_eq!(
            by_employee
                .iter()
                .map(|r| r.expense_id.as_str())
                .collect::<Vec<_>>(),
            vec!["EXP-3", "EXP-1"]
        );

        let by_expense = read_newest(
            &mut host,
            MAP,
            &AuditFilter {
                expense_id: Some("EXP-2".to_string()),
                ..AuditFilter::default()
            },
        )
        .unwrap();
        assert_eq!(by_expense.len(), 1);
        assert_eq!(by_expense[0].expense_id, "EXP-2");
    }

    #[test]
    fn read_newest_on_an_empty_ledger_returns_nothing() {
        let mut host = MockHost::new();
        assert!(read_newest(&mut host, MAP, &AuditFilter::default())
            .unwrap()
            .is_empty());
    }

    #[test]
    fn resolved_limit_clamps_to_the_documented_bounds() {
        assert_eq!(AuditFilter::default().resolved_limit(), DEFAULT_READ_LIMIT);
        assert_eq!(
            AuditFilter {
                limit: Some(0),
                ..AuditFilter::default()
            }
            .resolved_limit(),
            1
        );
        assert_eq!(
            AuditFilter {
                limit: Some(10_000),
                ..AuditFilter::default()
            }
            .resolved_limit(),
            MAX_READ_LIMIT
        );
    }

    #[test]
    fn find_duplicate_matches_all_four_fields_inside_the_window() {
        let mut host = MockHost::new();
        host.seed_json(
            MAP,
            b"exp:EXP-1042",
            serde_json::to_value(pointer("EXP-1042", NOW - 3 * DAY)).unwrap(),
        );
        let candidate = DuplicateCandidate {
            employee_ref: "emp_7f3a".to_string(),
            amount: 812.4,
            currency: "eur".to_string(),
            vendor: "Lufthansa".to_string(),
        };
        assert!(find_duplicate(&mut host, MAP, &candidate, 30, NOW).unwrap());
    }

    #[test]
    fn find_duplicate_rejects_a_different_field_or_an_old_expense() {
        let mut host = MockHost::new();
        host.seed_json(
            MAP,
            b"exp:EXP-1042",
            serde_json::to_value(pointer("EXP-1042", NOW - 3 * DAY)).unwrap(),
        );
        let base = DuplicateCandidate {
            employee_ref: "emp_7f3a".to_string(),
            amount: 812.4,
            currency: "EUR".to_string(),
            vendor: "Lufthansa".to_string(),
        };

        let other_vendor = DuplicateCandidate {
            vendor: "Air France".to_string(),
            ..base.clone()
        };
        assert!(!find_duplicate(&mut host, MAP, &other_vendor, 30, NOW).unwrap());

        let other_amount = DuplicateCandidate {
            amount: 100.0,
            ..base.clone()
        };
        assert!(!find_duplicate(&mut host, MAP, &other_amount, 30, NOW).unwrap());

        let other_employee = DuplicateCandidate {
            employee_ref: "emp_other".to_string(),
            ..base.clone()
        };
        assert!(!find_duplicate(&mut host, MAP, &other_employee, 30, NOW).unwrap());

        let other_currency = DuplicateCandidate {
            currency: "GBP".to_string(),
            ..base.clone()
        };
        assert!(!find_duplicate(&mut host, MAP, &other_currency, 30, NOW).unwrap());

        // Same claim but outside the window.
        assert!(!find_duplicate(&mut host, MAP, &base, 1, NOW).unwrap());
    }

    #[test]
    fn find_duplicate_tolerates_a_corrupt_index_entry() {
        let mut host = MockHost::new();
        host.seed(MAP, b"exp:BROKEN", b"{not json");
        let candidate = DuplicateCandidate {
            employee_ref: "emp_7f3a".to_string(),
            amount: 812.4,
            currency: "EUR".to_string(),
            vendor: "Lufthansa".to_string(),
        };
        assert!(!find_duplicate(&mut host, MAP, &candidate, 30, NOW).unwrap());
    }

    #[test]
    fn audit_record_json_shape_is_the_frozen_one() {
        let mut host = MockHost::new();
        let mut record = sample_record(0, "EXP-1042", "emp_7f3a");
        append(&mut host, MAP, &mut record).unwrap();

        // The stored bytes keep the frozen key order (a `Value` round-trip would
        // sort them, so the order is asserted on the raw record).
        let raw = host.value_at(MAP, b"seq:000001").unwrap();
        let text = String::from_utf8(raw.clone()).unwrap();
        let mut cursor = 0;
        for key in [
            "seq",
            "expense_id",
            "employee_ref",
            "verdict",
            "base_amount",
            "category",
            "vendor",
            "rule_hits",
            "recorded_at",
            "contract_version",
        ] {
            let needle = format!("\"{key}\":");
            let position = text[cursor..]
                .find(&needle)
                .unwrap_or_else(|| panic!("{needle} missing or out of order in {text}"));
            cursor += position + needle.len();
        }

        let stored: serde_json::Value = host.json_at(MAP, b"seq:000001").unwrap();
        assert_eq!(stored["rule_hits"][0], "over_approval_threshold");
        assert_eq!(stored["base_amount"], 884.52);
        assert_eq!(stored["recorded_at"], NOW);
    }

    #[test]
    fn audit_record_serialises_unknown_amounts_as_null() {
        let mut record = sample_record(0, "EXP-1042", "emp_7f3a");
        record.base_amount = None;
        let value = serde_json::to_value(&record).unwrap();
        assert!(value["base_amount"].is_null());
    }

    #[test]
    fn kv_failures_surface_with_the_kv_prefix() {
        let mut host = MockHost::new();
        host.kv_scan_failures
            .insert(MAP.to_string(), "write conflict".to_string());
        let error = highest_seq(&mut host, MAP).unwrap_err();
        assert!(error.starts_with(errors::KV), "{error}");
        assert!(error.contains("write conflict"));

        host.kv_put_failures
            .insert(MAP.to_string(), "write conflict".to_string());
        let mut record = sample_record(0, "EXP-1042", "emp_7f3a");
        let error = append(&mut host, MAP, &mut record).unwrap_err();
        assert!(error.starts_with(errors::KV), "{error}");
    }
}
