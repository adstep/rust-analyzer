use hir::{Either, FromSource, Module, ModuleSource, Path, PathResolution, Source, SourceAnalyzer};
use ra_db::FileId;
use ra_syntax::{ast, match_ast, AstNode, AstPtr};

use super::{
    name_definition::{from_assoc_item, from_module_def, from_pat, from_struct_field},
    NameDefinition, NameKind,
};
use crate::db::RootDatabase;

pub(crate) fn classify_name(
    db: &RootDatabase,
    file_id: FileId,
    name: &ast::Name,
) -> Option<NameDefinition> {
    let parent = name.syntax().parent()?;
    let file_id = file_id.into();

    // FIXME: add ast::MacroCall(it)
    match_ast! {
        match parent {
            ast::BindPat(it) => {
                from_pat(db, file_id, AstPtr::new(&it))
            },
            ast::RecordFieldDef(it) => {
                let ast = hir::FieldSource::Named(it);
                let src = hir::Source { file_id, ast };
                let field = hir::StructField::from_source(db, src)?;
                Some(from_struct_field(db, field))
            },
            ast::Module(it) => {
                let ast = hir::ModuleSource::Module(it);
                let src = hir::Source { file_id, ast };
                let def = hir::Module::from_definition(db, src)?;
                Some(from_module_def(db, def.into(), None))
            },
            ast::StructDef(it) => {
                let src = hir::Source { file_id, ast: it };
                let def = hir::Struct::from_source(db, src)?;
                Some(from_module_def(db, def.into(), None))
            },
            ast::EnumDef(it) => {
                let src = hir::Source { file_id, ast: it };
                let def = hir::Enum::from_source(db, src)?;
                Some(from_module_def(db, def.into(), None))
            },
            ast::TraitDef(it) => {
                let src = hir::Source { file_id, ast: it };
                let def = hir::Trait::from_source(db, src)?;
                Some(from_module_def(db, def.into(), None))
            },
            ast::StaticDef(it) => {
                let src = hir::Source { file_id, ast: it };
                let def = hir::Static::from_source(db, src)?;
                Some(from_module_def(db, def.into(), None))
            },
            ast::EnumVariant(it) => {
                let src = hir::Source { file_id, ast: it };
                let def = hir::EnumVariant::from_source(db, src)?;
                Some(from_module_def(db, def.into(), None))
            },
            ast::FnDef(it) => {
                let src = hir::Source { file_id, ast: it };
                let def = hir::Function::from_source(db, src)?;
                if parent.parent().and_then(ast::ItemList::cast).is_some() {
                    Some(from_assoc_item(db, def.into()))
                } else {
                    Some(from_module_def(db, def.into(), None))
                }
            },
            ast::ConstDef(it) => {
                let src = hir::Source { file_id, ast: it };
                let def = hir::Const::from_source(db, src)?;
                if parent.parent().and_then(ast::ItemList::cast).is_some() {
                    Some(from_assoc_item(db, def.into()))
                } else {
                    Some(from_module_def(db, def.into(), None))
                }
            },
            ast::TypeAliasDef(it) => {
                let src = hir::Source { file_id, ast: it };
                let def = hir::TypeAlias::from_source(db, src)?;
                if parent.parent().and_then(ast::ItemList::cast).is_some() {
                    Some(from_assoc_item(db, def.into()))
                } else {
                    Some(from_module_def(db, def.into(), None))
                }
            },
            _ => None,
        }
    }
}

pub(crate) fn classify_name_ref(
    db: &RootDatabase,
    file_id: FileId,
    name_ref: &ast::NameRef,
) -> Option<NameDefinition> {
    use PathResolution::*;

    let parent = name_ref.syntax().parent()?;
    let analyzer = SourceAnalyzer::new(db, file_id, name_ref.syntax(), None);

    if let Some(method_call) = ast::MethodCallExpr::cast(parent.clone()) {
        let func = analyzer.resolve_method_call(&method_call)?;
        return Some(from_assoc_item(db, func.into()));
    }

    if let Some(field_expr) = ast::FieldExpr::cast(parent.clone()) {
        if let Some(field) = analyzer.resolve_field(&field_expr) {
            return Some(from_struct_field(db, field));
        }
    }

    if let Some(record_field) = ast::RecordField::cast(parent.clone()) {
        if let Some(record_lit) = record_field.syntax().ancestors().find_map(ast::RecordLit::cast) {
            let variant_def = analyzer.resolve_record_literal(&record_lit)?;
            let hir_path = Path::from_name_ref(name_ref);
            let hir_name = hir_path.as_ident()?;
            let field = variant_def.field(db, hir_name)?;
            return Some(from_struct_field(db, field));
        }
    }

    let ast = ModuleSource::from_child_node(db, file_id, &parent);
    let file_id = file_id.into();
    // FIXME: find correct container and visibility for each case
    let container = Module::from_definition(db, Source { file_id, ast })?;
    let visibility = None;

    if let Some(macro_call) =
        parent.parent().and_then(|node| node.parent()).and_then(ast::MacroCall::cast)
    {
        if let Some(macro_def) = analyzer.resolve_macro_call(db, &macro_call) {
            return Some(NameDefinition {
                item: NameKind::Macro(macro_def),
                container,
                visibility,
            });
        }
    }

    let path = name_ref.syntax().ancestors().find_map(ast::Path::cast)?;
    let resolved = analyzer.resolve_path(db, &path)?;
    match resolved {
        Def(def) => Some(from_module_def(db, def, Some(container))),
        AssocItem(item) => Some(from_assoc_item(db, item)),
        LocalBinding(Either::A(pat)) => from_pat(db, file_id, pat),
        LocalBinding(Either::B(par)) => {
            let item = NameKind::SelfParam(par);
            Some(NameDefinition { item, container, visibility })
        }
        GenericParam(par) => {
            // FIXME: get generic param def
            let item = NameKind::GenericParam(par);
            Some(NameDefinition { item, container, visibility })
        }
        Macro(def) => {
            let item = NameKind::Macro(def);
            Some(NameDefinition { item, container, visibility })
        }
        SelfType(impl_block) => {
            let ty = impl_block.target_ty(db);
            let item = NameKind::SelfType(ty);
            let container = impl_block.module();
            Some(NameDefinition { item, container, visibility })
        }
    }
}
