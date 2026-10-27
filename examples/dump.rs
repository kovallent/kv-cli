fn main() {
    let src = std::fs::read_to_string(std::env::args().nth(1).unwrap()).unwrap();
    let mut p = tree_sitter::Parser::new();
    p.set_language(&tree_sitter_python::LANGUAGE.into())
        .unwrap();
    let tree = p.parse(&src, None).unwrap();
    let mut c = tree.walk();
    let mut depth = 0;
    loop {
        let n = c.node();
        let field = c.field_name().map(|f| format!("{f}: ")).unwrap_or_default();
        let text = if n.child_count() == 0 {
            format!("  {:?}", &src[n.byte_range()])
        } else {
            String::new()
        };
        println!("{}{}{}{}", "  ".repeat(depth), field, n.kind(), text);
        if c.goto_first_child() {
            depth += 1;
            continue;
        }
        loop {
            if c.goto_next_sibling() {
                break;
            }
            if !c.goto_parent() {
                return;
            }
            depth -= 1;
        }
    }
}
