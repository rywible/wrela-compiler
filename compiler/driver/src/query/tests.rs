use super::*;

struct Text;
impl Input for Text {
    type Key = u32;
    type Value = String;
    const NAME: &'static str = "text";
}

struct Len;
impl Query for Len {
    type Key = u32;
    type Value = usize;
    const NAME: &'static str = "len";
    fn compute(db: &Db, key: &u32) -> usize {
        db.input::<Text>(key).len()
    }
}

struct Total;
impl Query for Total {
    type Key = ();
    type Value = usize;
    const NAME: &'static str = "total";
    fn compute(db: &Db, _: &()) -> usize {
        db.get::<Len>(&0) + db.get::<Len>(&1)
    }
}

struct Loop;
impl Query for Loop {
    type Key = u32;
    type Value = u32;
    const NAME: &'static str = "loop";
    fn compute(db: &Db, key: &u32) -> u32 {
        db.get::<Loop>(&(1 - key))
    }
}

#[test]
fn memoizes_within_a_revision() {
    let db = Db::new();
    db.set_input::<Text>(0, "ab".into());
    db.set_input::<Text>(1, "cde".into());
    assert_eq!(db.get::<Total>(&()), 5);
    assert_eq!(db.get::<Total>(&()), 5);
    assert_eq!(db.runs::<Total>(), 1);
    assert_eq!(db.runs::<Len>(), 2);
}

#[test]
fn recomputes_only_what_changed() {
    let db = Db::new();
    db.set_input::<Text>(0, "ab".into());
    db.set_input::<Text>(1, "cde".into());
    db.get::<Total>(&());
    db.set_input::<Text>(1, "cdef".into());
    assert_eq!(db.get::<Total>(&()), 6);
    assert_eq!(db.runs::<Len>(), 3, "only Len(1) reran");
    assert_eq!(db.runs::<Total>(), 2);
}

#[test]
fn early_cutoff_stops_at_an_equal_value() {
    let db = Db::new();
    db.set_input::<Text>(0, "ab".into());
    db.set_input::<Text>(1, "cde".into());
    db.get::<Total>(&());
    db.set_input::<Text>(1, "xyz".into()); // same length
    assert_eq!(db.get::<Total>(&()), 5);
    assert_eq!(db.runs::<Len>(), 3);
    assert_eq!(db.runs::<Total>(), 1, "Len(1) didn't change, so Total was reused");
}

#[test]
fn setting_an_equal_input_is_not_a_change() {
    let db = Db::new();
    db.set_input::<Text>(0, "ab".into());
    let r = db.revision();
    db.set_input::<Text>(0, "ab".into());
    assert_eq!(db.revision(), r);
}

#[test]
#[should_panic(expected = "query cycle")]
fn cycles_are_detected() {
    let db = Db::new();
    db.get::<Loop>(&0);
}
