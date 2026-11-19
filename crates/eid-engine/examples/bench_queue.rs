//! Cost of the queue bookkeeping with many items (benchmark tool, not a test gate;
//! `scripts/bench/`): `cargo run --release -p eid-engine --example bench_queue -- DIR N...`.
//! For each N it builds a queue of N items, then times (median of 9 runs) the full-snapshot
//! clone done on every window poll and every state change, its JSON serialization, and
//! `save_queues` (what `persist` does on every state change: write + fsync + rename).
use eid_engine::queue::{Item, Queue, QueueSet, Status};
use eid_engine::scheduler::save_queues;
use std::time::Instant;

fn median(mut v: Vec<f64>) -> f64 {
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    v[v.len() / 2]
}

fn main() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let dir = std::path::PathBuf::from(&a[0]);
    std::fs::create_dir_all(&dir).unwrap();
    println!("| items | clone ms | to_json ms | json KiB | save_queues ms (write+fsync+rename) |");
    println!("|---:|---:|---:|---:|---:|");
    for n in a[1..].iter().map(|s| s.parse::<u64>().unwrap()) {
        let mut q = Queue::new("main", "/home/user/Downloads");
        for id in 1..=n {
            q.items.push(Item {
                id,
                url: format!(
                    "https://downloads.example.com/releases/v{id}/package-{id}-x86_64.tar.zst"
                ),
                subdir: None,
                file_name: Some(format!("package-{id}-x86_64.tar.zst")),
                status: if id % 3 == 0 {
                    Status::Done
                } else {
                    Status::Pending
                },
                attempts: 0,
                not_before: None,
                last_error: None,
                limit_kib: None,
            });
        }
        let qs = QueueSet {
            queues: vec![q],
            next_id: n + 1,
        };
        let path = dir.join(format!("queues-{n}.json"));
        let (mut c, mut j, mut s) = (vec![], vec![], vec![]);
        let mut size = 0;
        for _ in 0..9 {
            let t = Instant::now();
            let cl = qs.clone();
            c.push(t.elapsed().as_secs_f64() * 1e3);
            let t = Instant::now();
            let js = serde_json::to_vec(&cl).unwrap();
            j.push(t.elapsed().as_secs_f64() * 1e3);
            size = js.len();
            let t = Instant::now();
            save_queues(&path, &cl).unwrap();
            s.push(t.elapsed().as_secs_f64() * 1e3);
        }
        println!(
            "| {n} | {:.3} | {:.3} | {} | {:.2} |",
            median(c),
            median(j),
            size / 1024,
            median(s)
        );
    }
}
