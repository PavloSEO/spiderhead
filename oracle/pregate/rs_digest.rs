// Digest every cell of every table: per-table sha256 over (type tag, canonical bytes).
use rusqlite::{types::ValueRef, Connection, OpenFlags};
use sha2::{Digest, Sha256};
fn main() {
    let path = std::env::args().nth(1).unwrap();
    let c = Connection::open_with_flags(&path, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    let v: String = c.query_row("select sqlite_version()", [], |r| r.get(0)).unwrap();
    eprintln!("sqlite {v}");
    let mut st = c.prepare("select name from sqlite_master where type='table' order by name").unwrap();
    let tables: Vec<String> = st.query_map([], |r| r.get(0)).unwrap().map(Result::unwrap).collect();
    for t in tables {
        let mut q = c.prepare(&format!("select * from \"{t}\" order by rowid")).unwrap();
        let n = q.column_count();
        let mut rows = q.query([]).unwrap();
        let (mut h, mut count) = (Sha256::new(), 0u64);
        while let Some(r) = rows.next().unwrap() {
            count += 1;
            for i in 0..n {
                match r.get_ref(i).unwrap() {
                    ValueRef::Null => h.update(b"N"),
                    ValueRef::Integer(x) => { h.update(b"I"); h.update(x.to_le_bytes()) }
                    ValueRef::Real(x) => { h.update(b"R"); h.update(x.to_bits().to_le_bytes()) }
                    ValueRef::Text(b) => { h.update(b"T"); h.update((b.len() as u64).to_le_bytes()); h.update(b) }
                    ValueRef::Blob(b) => { h.update(b"B"); h.update((b.len() as u64).to_le_bytes()); h.update(b) }
                }
            }
        }
        println!("{t}\t{count}\t{:x}", h.finalize());
    }
}
