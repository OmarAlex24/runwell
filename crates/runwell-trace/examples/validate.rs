//! Parses a trace file and prints how many job records it holds.
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::args()
        .nth(1)
        .ok_or("usage: validate <trace.jsonl>")?;
    let jobs = runwell_trace::read_jsonl(std::io::BufReader::new(std::fs::File::open(path)?))?;
    println!("parsed {} jobs", jobs.len());
    Ok(())
}
