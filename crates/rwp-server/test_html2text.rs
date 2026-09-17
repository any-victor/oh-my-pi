use html2text;

fn main() {
    let html = "<!doctype html><html><head><title>Ignored</title></head><body><article><h1>Readable Heading</h1><p>Readable body text.</p></article></body></html>";
    let result = html2text::from_read(html.as_bytes(), 120);
    println!("Result: {:?}", result);
}
