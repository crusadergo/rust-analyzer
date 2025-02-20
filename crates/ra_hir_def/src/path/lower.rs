//! Transforms syntax into `Path` objects, ideally with accounting for hygiene

mod lower_use;

use std::sync::Arc;

use either::Either;
use hir_expand::{
    hygiene::Hygiene,
    name::{name, AsName},
};
use ra_syntax::ast::{self, AstNode, TypeAscriptionOwner};

use crate::{
    path::{GenericArg, GenericArgs, ModPath, Path, PathKind},
    type_ref::TypeRef,
};

pub(super) use lower_use::lower_use_tree;

/// Converts an `ast::Path` to `Path`. Works with use trees.
/// It correctly handles `$crate` based path from macro call.
pub(super) fn lower_path(mut path: ast::Path, hygiene: &Hygiene) -> Option<Path> {
    let mut kind = PathKind::Plain;
    let mut segments = Vec::new();
    let mut generic_args = Vec::new();
    loop {
        let segment = path.segment()?;

        if segment.has_colon_colon() {
            kind = PathKind::Abs;
        }

        match segment.kind()? {
            ast::PathSegmentKind::Name(name_ref) => {
                // FIXME: this should just return name
                match hygiene.name_ref_to_name(name_ref) {
                    Either::Left(name) => {
                        let args = segment
                            .type_arg_list()
                            .and_then(lower_generic_args)
                            .or_else(|| {
                                lower_generic_args_from_fn_path(
                                    segment.param_list(),
                                    segment.ret_type(),
                                )
                            })
                            .map(Arc::new);
                        segments.push(name);
                        generic_args.push(args)
                    }
                    Either::Right(crate_id) => {
                        kind = PathKind::DollarCrate(crate_id);
                        break;
                    }
                }
            }
            ast::PathSegmentKind::Type { type_ref, trait_ref } => {
                assert!(path.qualifier().is_none()); // this can only occur at the first segment

                let self_type = TypeRef::from_ast(type_ref?);

                match trait_ref {
                    // <T>::foo
                    None => {
                        kind = PathKind::Type(Box::new(self_type));
                    }
                    // <T as Trait<A>>::Foo desugars to Trait<Self=T, A>::Foo
                    Some(trait_ref) => {
                        let path = Path::from_src(trait_ref.path()?, hygiene)?;
                        kind = path.mod_path.kind;

                        let mut prefix_segments = path.mod_path.segments;
                        prefix_segments.reverse();
                        segments.extend(prefix_segments);

                        let mut prefix_args = path.generic_args;
                        prefix_args.reverse();
                        generic_args.extend(prefix_args);

                        // Insert the type reference (T in the above example) as Self parameter for the trait
                        let last_segment = generic_args.last_mut()?;
                        if last_segment.is_none() {
                            *last_segment = Some(Arc::new(GenericArgs::empty()));
                        };
                        let args = last_segment.as_mut().unwrap();
                        let mut args_inner = Arc::make_mut(args);
                        args_inner.has_self_type = true;
                        args_inner.args.insert(0, GenericArg::Type(self_type));
                    }
                }
            }
            ast::PathSegmentKind::CrateKw => {
                kind = PathKind::Crate;
                break;
            }
            ast::PathSegmentKind::SelfKw => {
                kind = PathKind::Self_;
                break;
            }
            ast::PathSegmentKind::SuperKw => {
                kind = PathKind::Super;
                break;
            }
        }
        path = match qualifier(&path) {
            Some(it) => it,
            None => break,
        };
    }
    segments.reverse();
    generic_args.reverse();
    let mod_path = ModPath { kind, segments };
    return Some(Path { mod_path, generic_args });

    fn qualifier(path: &ast::Path) -> Option<ast::Path> {
        if let Some(q) = path.qualifier() {
            return Some(q);
        }
        // FIXME: this bottom up traversal is not too precise.
        // Should we handle do a top-down analysis, recording results?
        let use_tree_list = path.syntax().ancestors().find_map(ast::UseTreeList::cast)?;
        let use_tree = use_tree_list.parent_use_tree();
        use_tree.path()
    }
}

pub(super) fn lower_generic_args(node: ast::TypeArgList) -> Option<GenericArgs> {
    let mut args = Vec::new();
    for type_arg in node.type_args() {
        let type_ref = TypeRef::from_ast_opt(type_arg.type_ref());
        args.push(GenericArg::Type(type_ref));
    }
    // lifetimes ignored for now
    let mut bindings = Vec::new();
    for assoc_type_arg in node.assoc_type_args() {
        if let Some(name_ref) = assoc_type_arg.name_ref() {
            let name = name_ref.as_name();
            let type_ref = TypeRef::from_ast_opt(assoc_type_arg.type_ref());
            bindings.push((name, type_ref));
        }
    }
    if args.is_empty() && bindings.is_empty() {
        None
    } else {
        Some(GenericArgs { args, has_self_type: false, bindings })
    }
}

/// Collect `GenericArgs` from the parts of a fn-like path, i.e. `Fn(X, Y)
/// -> Z` (which desugars to `Fn<(X, Y), Output=Z>`).
fn lower_generic_args_from_fn_path(
    params: Option<ast::ParamList>,
    ret_type: Option<ast::RetType>,
) -> Option<GenericArgs> {
    let mut args = Vec::new();
    let mut bindings = Vec::new();
    if let Some(params) = params {
        let mut param_types = Vec::new();
        for param in params.params() {
            let type_ref = TypeRef::from_ast_opt(param.ascribed_type());
            param_types.push(type_ref);
        }
        let arg = GenericArg::Type(TypeRef::Tuple(param_types));
        args.push(arg);
    }
    if let Some(ret_type) = ret_type {
        let type_ref = TypeRef::from_ast_opt(ret_type.type_ref());
        bindings.push((name![Output], type_ref))
    }
    if args.is_empty() && bindings.is_empty() {
        None
    } else {
        Some(GenericArgs { args, has_self_type: false, bindings })
    }
}
