use std::iter::successors;

use ast::{edit::AstNodeEdit, make};
use ra_syntax::{ast, AstNode, AstToken, Direction, InsertPosition, SyntaxElement, T};

use crate::{Assist, AssistCtx, AssistId};

// Assist: merge_imports
//
// Merges two imports with a common prefix.
//
// ```
// use std::<|>fmt::Formatter;
// use std::io;
// ```
// ->
// ```
// use std::{fmt::Formatter, io};
// ```
pub(crate) fn merge_imports(ctx: AssistCtx) -> Option<Assist> {
    let tree: ast::UseTree = ctx.find_node_at_offset()?;
    let use_item = tree.syntax().parent().and_then(ast::UseItem::cast)?;
    let (merged, to_delete) = [Direction::Prev, Direction::Next]
        .iter()
        .copied()
        .filter_map(|dir| next_use_item(&use_item, dir))
        .filter_map(|it| Some((it.clone(), it.use_tree()?)))
        .find_map(|(use_item, use_tree)| {
            Some((try_merge_trees(&tree, &use_tree)?, use_item.clone()))
        })?;
    let mut offset = ctx.frange.range.start();
    ctx.add_assist(AssistId("merge_imports"), "Merge imports", |edit| {
        edit.replace_ast(tree, merged);

        let mut range = to_delete.syntax().text_range();
        let next_ws = to_delete
            .syntax()
            .next_sibling_or_token()
            .and_then(|it| it.into_token())
            .and_then(ast::Whitespace::cast);
        if let Some(ws) = next_ws {
            range = range.extend_to(&ws.syntax().text_range())
        }
        edit.delete(range);
        if range.end() <= offset {
            offset -= range.len();
        }
        edit.set_cursor(offset);
    })
}

fn next_use_item(this_use_item: &ast::UseItem, direction: Direction) -> Option<ast::UseItem> {
    this_use_item.syntax().siblings(direction).skip(1).find_map(ast::UseItem::cast)
}

fn try_merge_trees(old: &ast::UseTree, new: &ast::UseTree) -> Option<ast::UseTree> {
    let lhs_path = old.path()?;
    let rhs_path = new.path()?;

    let (lhs_prefix, rhs_prefix) = common_prefix(&lhs_path, &rhs_path)?;

    let lhs = old.split_prefix(&lhs_prefix);
    let rhs = new.split_prefix(&rhs_prefix);

    let mut to_insert: Vec<SyntaxElement> = Vec::new();
    to_insert.push(make::token(T![,]).into());
    to_insert.push(make::tokens::single_space().into());
    to_insert.extend(
        rhs.use_tree_list()?
            .syntax()
            .children_with_tokens()
            .filter(|it| it.kind() != T!['{'] && it.kind() != T!['}']),
    );
    let use_tree_list = lhs.use_tree_list()?;
    let pos = InsertPosition::Before(use_tree_list.r_curly()?.into());
    let use_tree_list = use_tree_list.insert_children(pos, to_insert);
    Some(lhs.with_use_tree_list(use_tree_list))
}

fn common_prefix(lhs: &ast::Path, rhs: &ast::Path) -> Option<(ast::Path, ast::Path)> {
    let mut res = None;
    let mut lhs_curr = first_path(&lhs);
    let mut rhs_curr = first_path(&rhs);
    loop {
        match (lhs_curr.segment(), rhs_curr.segment()) {
            (Some(lhs), Some(rhs)) if lhs.syntax().text() == rhs.syntax().text() => (),
            _ => break,
        }
        res = Some((lhs_curr.clone(), rhs_curr.clone()));

        match (lhs_curr.parent_path(), rhs_curr.parent_path()) {
            (Some(lhs), Some(rhs)) => {
                lhs_curr = lhs;
                rhs_curr = rhs;
            }
            _ => break,
        }
    }

    res
}

fn first_path(path: &ast::Path) -> ast::Path {
    successors(Some(path.clone()), |it| it.qualifier()).last().unwrap()
}

#[cfg(test)]
mod tests {
    use crate::helpers::check_assist;

    use super::*;

    #[test]
    fn test_merge_first() {
        check_assist(
            merge_imports,
            r"
use std::fmt<|>::Debug;
use std::fmt::Display;
",
            r"
use std::fmt<|>::{Debug, Display};
",
        )
    }

    #[test]
    fn test_merge_second() {
        check_assist(
            merge_imports,
            r"
use std::fmt::Debug;
use std::fmt<|>::Display;
",
            r"
use std::fmt<|>::{Display, Debug};
",
        )
    }

    #[test]
    #[ignore]
    fn test_merge_nested() {
        check_assist(
            merge_imports,
            r"
use std::{fmt<|>::Debug, fmt::Display};
",
            r"
use std::{fmt::{Debug, Display}};
",
        )
    }
}
