//! 显式 KV 源验证器 (explicit KV source validator).
//!
//! 设计要点:
//!
//! - 通过 `Source::visit` 恰好访问同源一次,保留访问次序与重复键;
//!   不调用 `Source::count`(其默认实现会再访问一次),也不从 `Debug`
//!   字符串解析任何字段。
//! - 用 `VisitValue` 判定 `Value` 种类并与允许键表核验;未知 opaque
//!   值落入 `visit_any`,只记类别,绝不调用 `Display`/`Debug` 强转。
//! - visitor 出错即停,但报告里保留已访问前缀的回执。
//! - 结果(`Report`/`Receipt`)只含类别与来源(allow-table 出处),不存值。
//! - 不改动默认 `log::Record` 行为。
//!
//! 局部运行命令 (local command):
//!
//! ```sh
//! cargo test --test kv_validator --features kv
//! ```
//!
//! 四字段例子见 `four_field_example`。

#![cfg(feature = "kv")]

use log::kv::{self, Key, Source, Value, VisitSource, VisitValue};
use std::cell::Cell;
use std::collections::BTreeMap;
use std::fmt;

/// 值的类别。验证器只保留类别,从不保留值本身。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Category {
    Null,
    Bool,
    I64,
    U64,
    I128,
    U128,
    F64,
    Str,
    /// 无法归入基本类别的值(如 `Value::from_debug`/`from_display` 捕获的值)。
    /// 归类时不调用 `Display`/`Debug` 强转。
    Opaque,
}

/// 允许键表中的一条:键名 + 期望的值类别。
struct KeySpec {
    key: &'static str,
    expect: Category,
}

/// 同名键的报告策略。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DuplicatePolicy {
    /// 重复键照常记录回执,不额外标记。
    Allow,
    /// 重复键保留在回执序列中,同时标记并列入 `Report::duplicates`。
    Flag,
}

/// 回执中键的来源(出处)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Origin {
    /// 命中允许键表,携带表内下标。
    Allowed(usize),
    /// 不在允许键表中。
    Unknown,
}

/// 单条已访问键值对的回执:只有次序、键名、类别与来源,没有值。
#[derive(Debug)]
struct Receipt {
    /// 访问次序(从 0 开始)。
    index: usize,
    key: String,
    category: Category,
    origin: Origin,
    /// 按 `DuplicatePolicy::Flag` 策略标记的重复键。
    duplicate: bool,
}

/// 键存在但值类别与允许键表不符。
#[derive(Debug)]
struct Mismatch {
    index: usize,
    key: String,
    expected: Category,
    actual: Category,
}

/// 验证结果。只含类别与来源信息,不存任何值。
#[derive(Debug, Default)]
struct Report {
    /// 已访问前缀的回执,按访问次序排列;visitor 失败时只含失败前的部分。
    receipts: Vec<Receipt>,
    /// 按策略报告出的重复键。
    duplicates: Vec<String>,
    mismatches: Vec<Mismatch>,
    unknown_keys: Vec<String>,
    /// 整个源是否被完整访问;visitor 出错提前停止时为 `false`。
    complete: bool,
    /// visitor 返回的错误(若有)。
    error: Option<String>,
}

/// 显式 KV 源验证器。
struct Validator {
    table: Vec<KeySpec>,
    policy: DuplicatePolicy,
    /// 测试钩子:在第 `n` 对(0 起)让 visitor 失败。
    fail_at: Option<usize>,
}

impl Validator {
    fn new(table: Vec<KeySpec>, policy: DuplicatePolicy) -> Self {
        Validator {
            table,
            policy,
            fail_at: None,
        }
    }

    /// 恰好访问源一次并产出报告。
    fn validate<S: Source + ?Sized>(&self, source: &S) -> Report {
        let mut visit = Visit {
            validator: self,
            seen: BTreeMap::new(),
            report: Report::default(),
        };

        // 同源只访问这一次;不调用 `count()` 等会再次访问的 API。
        match source.visit(&mut visit) {
            Ok(()) => visit.report.complete = true,
            Err(err) => {
                visit.report.complete = false;
                visit.report.error = Some(err.to_string());
            }
        }

        visit.report
    }
}

struct Visit<'a> {
    validator: &'a Validator,
    seen: BTreeMap<String, usize>,
    report: Report,
}

impl<'kvs> VisitSource<'kvs> for Visit<'_> {
    fn visit_pair(&mut self, key: Key<'kvs>, value: Value<'kvs>) -> Result<(), kv::Error> {
        let index = self.report.receipts.len();

        if self.validator.fail_at == Some(index) {
            return Err(kv::Error::msg("injected visitor failure"));
        }

        let key_name = key.as_str();
        let category = categorize(&value);

        let origin = match self
            .validator
            .table
            .iter()
            .position(|spec| spec.key == key_name)
        {
            Some(i) => Origin::Allowed(i),
            None => {
                self.report.unknown_keys.push(key_name.to_string());
                Origin::Unknown
            }
        };

        if let Origin::Allowed(i) = origin {
            let expected = self.validator.table[i].expect;
            if expected != category {
                self.report.mismatches.push(Mismatch {
                    index,
                    key: key_name.to_string(),
                    expected,
                    actual: category,
                });
            }
        }

        let times_seen = self.seen.entry(key_name.to_string()).or_insert(0);
        *times_seen += 1;
        let is_repeat = *times_seen > 1;
        let flagged = is_repeat && self.validator.policy == DuplicatePolicy::Flag;
        if flagged {
            self.report.duplicates.push(key_name.to_string());
        }

        self.report.receipts.push(Receipt {
            index,
            key: key_name.to_string(),
            category,
            origin,
            duplicate: flagged,
        });

        Ok(())
    }
}

/// 用 `VisitValue` 判定类别;opaque 值走 `visit_any`,不做任何格式化。
fn categorize(value: &Value) -> Category {
    struct Categorize(Category);

    impl<'v> VisitValue<'v> for Categorize {
        fn visit_any(&mut self, _: Value) -> Result<(), kv::Error> {
            // 未知 opaque 值:不调用 Display/Debug 强转,只记类别。
            self.0 = Category::Opaque;
            Ok(())
        }

        fn visit_null(&mut self) -> Result<(), kv::Error> {
            self.0 = Category::Null;
            Ok(())
        }

        fn visit_bool(&mut self, _: bool) -> Result<(), kv::Error> {
            self.0 = Category::Bool;
            Ok(())
        }

        fn visit_i64(&mut self, _: i64) -> Result<(), kv::Error> {
            self.0 = Category::I64;
            Ok(())
        }

        fn visit_u64(&mut self, _: u64) -> Result<(), kv::Error> {
            self.0 = Category::U64;
            Ok(())
        }

        fn visit_i128(&mut self, _: i128) -> Result<(), kv::Error> {
            self.0 = Category::I128;
            Ok(())
        }

        fn visit_u128(&mut self, _: u128) -> Result<(), kv::Error> {
            self.0 = Category::U128;
            Ok(())
        }

        fn visit_f64(&mut self, _: f64) -> Result<(), kv::Error> {
            self.0 = Category::F64;
            Ok(())
        }

        fn visit_str(&mut self, _: &str) -> Result<(), kv::Error> {
            self.0 = Category::Str;
            Ok(())
        }

        fn visit_borrowed_str(&mut self, _: &'v str) -> Result<(), kv::Error> {
            self.0 = Category::Str;
            Ok(())
        }
    }

    let mut categorize = Categorize(Category::Opaque);
    // 基本类别的 visit 不会失败;opaque 也不会在这里格式化。
    let _ = value.visit(&mut categorize);
    categorize.0
}

/// 空源合法:没有任何键值对也能通过验证。
#[test]
fn empty_source_is_valid() {
    let validator = Validator::new(
        vec![KeySpec {
            key: "a",
            expect: Category::I64,
        }],
        DuplicatePolicy::Flag,
    );

    let source: &[(&str, i64)] = &[];
    let report = validator.validate(source);

    assert!(report.complete);
    assert!(report.error.is_none());
    assert!(report.receipts.is_empty());
    assert!(report.duplicates.is_empty());
    assert!(report.mismatches.is_empty());
    assert!(report.unknown_keys.is_empty());
}

/// 四字段例子:msg/id/ratio/ok 四个字段按允许键表核验通过。
#[test]
fn four_field_example() {
    let validator = Validator::new(
        vec![
            KeySpec {
                key: "msg",
                expect: Category::Str,
            },
            KeySpec {
                key: "id",
                expect: Category::U64,
            },
            KeySpec {
                key: "ratio",
                expect: Category::F64,
            },
            KeySpec {
                key: "ok",
                expect: Category::Bool,
            },
        ],
        DuplicatePolicy::Flag,
    );

    let source = &[
        ("msg", Value::from("hello")),
        ("id", Value::from(7u64)),
        ("ratio", Value::from(0.5f64)),
        ("ok", Value::from(true)),
    ];
    let report = validator.validate(source);

    assert!(report.complete);
    assert_eq!(4, report.receipts.len());
    assert_eq!(
        vec![
            Category::Str,
            Category::U64,
            Category::F64,
            Category::Bool
        ],
        report
            .receipts
            .iter()
            .map(|receipt| receipt.category)
            .collect::<Vec<_>>()
    );
    // 每个键都来自允许键表,且下标与表内位置一致。
    for (i, receipt) in report.receipts.iter().enumerate() {
        assert_eq!(Origin::Allowed(i), receipt.origin);
        assert_eq!(i, receipt.index);
        assert!(!receipt.duplicate);
    }
    assert!(report.duplicates.is_empty());
    assert!(report.mismatches.is_empty());
    assert!(report.unknown_keys.is_empty());
}

/// 两个同名键按策略报告为重复,且访问次序保留。
#[test]
fn duplicate_keys_flagged_per_policy() {
    let table = || {
        vec![KeySpec {
            key: "a",
            expect: Category::I64,
        }]
    };
    let source = &[("a", Value::from(1i64)), ("a", Value::from(2i64))];

    // Flag 策略:第二次出现被标记并列入 duplicates,两条回执都保留。
    let report = Validator::new(table(), DuplicatePolicy::Flag).validate(source);
    assert!(report.complete);
    assert_eq!(2, report.receipts.len());
    assert_eq!(
        vec![0, 1],
        report.receipts.iter().map(|r| r.index).collect::<Vec<_>>()
    );
    assert!(!report.receipts[0].duplicate);
    assert!(report.receipts[1].duplicate);
    assert_eq!(vec!["a".to_string()], report.duplicates);

    // Allow 策略:不标记、不列入 duplicates,但回执仍按序保留。
    let report = Validator::new(table(), DuplicatePolicy::Allow).validate(source);
    assert!(report.complete);
    assert_eq!(2, report.receipts.len());
    assert!(report.receipts.iter().all(|r| !r.duplicate));
    assert!(report.duplicates.is_empty());
}

/// 未知 opaque 值只记类别,`Display`/`Debug` 调用次数为 0。
#[test]
fn opaque_value_display_is_never_called() {
    struct Counted<'a> {
        displays: &'a Cell<usize>,
        debugs: &'a Cell<usize>,
    }

    impl fmt::Display for Counted<'_> {
        fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
            self.displays.set(self.displays.get() + 1);
            f.write_str("counted")
        }
    }

    impl fmt::Debug for Counted<'_> {
        fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
            self.debugs.set(self.debugs.get() + 1);
            f.write_str("Counted")
        }
    }

    let displays = Cell::new(0);
    let debugs = Cell::new(0);
    let counted = Counted {
        displays: &displays,
        debugs: &debugs,
    };

    let validator = Validator::new(
        vec![KeySpec {
            key: "opaque",
            expect: Category::Opaque,
        }],
        DuplicatePolicy::Flag,
    );

    let source = &[("opaque", Value::from_display(&counted))];
    let report = validator.validate(source);

    assert!(report.complete);
    assert_eq!(1, report.receipts.len());
    assert_eq!(Category::Opaque, report.receipts[0].category);
    assert!(report.mismatches.is_empty());
    assert_eq!(
        0,
        displays.get(),
        "Display must not be called on opaque values"
    );
    assert_eq!(0, debugs.get(), "Debug must not be called on opaque values");
}

/// 第三对处 visitor 失败:前两对有完整回执,整体回执标记为未完整。
#[test]
fn visitor_error_stops_and_reports_visited_prefix() {
    let mut validator = Validator::new(
        vec![
            KeySpec {
                key: "a",
                expect: Category::I64,
            },
            KeySpec {
                key: "b",
                expect: Category::I64,
            },
            KeySpec {
                key: "c",
                expect: Category::I64,
            },
            KeySpec {
                key: "d",
                expect: Category::I64,
            },
        ],
        DuplicatePolicy::Flag,
    );
    // 在第三对(下标 2)注入 visitor 失败。
    validator.fail_at = Some(2);

    let source = &[
        ("a", Value::from(1i64)),
        ("b", Value::from(2i64)),
        ("c", Value::from(3i64)),
        ("d", Value::from(4i64)),
    ];
    let report = validator.validate(&source);

    // 前两对有完整回执,整体未完整。
    assert!(!report.complete);
    assert!(report.error.is_some());
    assert_eq!(2, report.receipts.len());
    assert_eq!("a", report.receipts[0].key);
    assert_eq!("b", report.receipts[1].key);
    assert_eq!(Category::I64, report.receipts[0].category);
    assert_eq!(Category::I64, report.receipts[1].category);
    assert!(report
        .receipts
        .iter()
        .all(|r| matches!(r.origin, Origin::Allowed(_))));
}

/// 同源只被访问一次。
#[test]
fn source_is_visited_exactly_once() {
    struct CountingSource<'a> {
        visits: &'a Cell<usize>,
    }

    impl Source for CountingSource<'_> {
        fn visit<'kvs>(&'kvs self, visitor: &mut dyn VisitSource<'kvs>) -> Result<(), kv::Error> {
            self.visits.set(self.visits.get() + 1);
            visitor.visit_pair(Key::from_str("a"), Value::from(1i64))
        }
    }

    let visits = Cell::new(0);
    let source = CountingSource { visits: &visits };

    let validator = Validator::new(
        vec![KeySpec {
            key: "a",
            expect: Category::I64,
        }],
        DuplicatePolicy::Flag,
    );
    let report = validator.validate(&source);

    assert!(report.complete);
    assert_eq!(1, report.receipts.len());
    assert_eq!(1, visits.get(), "the source must be visited exactly once");
}

/// 未知键与类别不符分别进入 unknown_keys 与 mismatches。
#[test]
fn unknown_key_and_category_mismatch_are_reported() {
    let validator = Validator::new(
        vec![KeySpec {
            key: "port",
            expect: Category::U64,
        }],
        DuplicatePolicy::Flag,
    );

    let source = &[("port", Value::from("8080")), ("extra", Value::from(1i64))];
    let report = validator.validate(source);

    assert!(report.complete);
    assert_eq!(2, report.receipts.len());
    assert_eq!(Origin::Allowed(0), report.receipts[0].origin);
    assert_eq!(Origin::Unknown, report.receipts[1].origin);
    assert_eq!(vec!["extra".to_string()], report.unknown_keys);
    assert_eq!(1, report.mismatches.len());
    assert_eq!(0, report.mismatches[0].index);
    assert_eq!("port", report.mismatches[0].key);
    assert_eq!(Category::U64, report.mismatches[0].expected);
    assert_eq!(Category::Str, report.mismatches[0].actual);
}

/// 验证器不改动默认 `log::Record`:默认 Record 的键值源仍为空。
#[test]
fn default_record_kv_is_untouched() {
    let record = log::Record::builder().build();
    assert_eq!(0, record.key_values().count());
}
