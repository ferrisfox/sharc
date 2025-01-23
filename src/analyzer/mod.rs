use std::collections::HashMap;

use crate::report::{Result, ReportKind, LogHandler};
use crate::span::Sp;
use crate::parser::ast;

mod hir;
use hir::{Node, ValId, Var, Type};

#[derive(Default)]
pub struct Analyzer {
	scope:   Vec<Scope>,
	symbols: HashMap<ValId, String>,
}

#[derive(Default)]
struct Scope {
	idacc:  ValId,
	locals: Vec<(ValId, String, Type)>, 
}

impl Scope {
	fn new_id(&mut self) -> ValId {
		self.idacc.0 += 1;
		self.idacc
	}
}

impl Analyzer {
	fn push_new_scope(&mut self) {
		self.scope.push(Scope::default());
	}

	fn pop_scope(&mut self) {
		self.scope.pop();
	}

	fn peek_scope_mut(&mut self) -> &mut Scope {
		self.scope.last_mut().unwrap()
	}

	fn peek_scope(&self) -> &Scope {
		self.scope.last().unwrap()
	}

	fn get_global_mut(&mut self) -> &mut Scope {
		self.scope.first_mut().unwrap()
	}

	fn get_global(&self) -> &Scope {
		self.scope.first().unwrap()
	}

	fn find_matching_descending<F: Fn((ValId, &str, &Type)) -> bool>(&self, f: F) 
		-> Option<(usize, &(ValId, String, Type))> {
		self.scope.iter().enumerate().rev().find_map(|(d, scope)| // TODO: mayhaps dont clone
			scope.locals.iter().rev().find(|(i,n,t)| f((*i,n,t))).map(|v| (d, v)))
	}

	pub fn analyze(ast: Vec<Sp<ast::Node>>, file: &'static str, handler: &LogHandler) -> (Vec<Node>, HashMap<ValId, String>) {
		let mut analyzer = Self {
			scope: vec![Scope::default()],
			symbols: HashMap::new(),
		};

		(ast.into_iter().fold(Vec::new(), |mut acc, node| {
			match analyzer.analyze_root(node) {
				Ok(n)  => acc.push(n),
				Err(e) => {
					handler.log(e.file(file));
					analyzer.scope.truncate(1);
				},
			}; acc
		}), analyzer.symbols)
	}

	fn analyze_root(&mut self, node: Sp<ast::Node>) -> Result<Node> {
		Ok(match node.elem {
			ast::Node::Func { name, args, ret, attrs, body } 
				if attrs.iter().any(|a| matches!(**a, ast::Attrs::Extern)) => {
				if !body.is_empty() {
					return ReportKind::SyntaxError // TODO: maybe not SyntaxError
						.title("Extern functions cannot have a body")
						.span(node.span)
						.as_err();
				}

				let id = self.peek_scope_mut().new_id();
				self.symbols.insert(id, name.elem.to_string());

				let mut nargs = Vec::new();
				let mut fargs = Vec::new();
				for (_, ty) in args {
					if matches!(*ty, ast::Type::Void) {
						return ReportKind::TypeError
							.title("Type 'void' is not allowed as a function argument")
							.help("Remove the arg, or change the type to '*void'")
							.span(ty.span)
							.as_err();
					}

					let ty = convert_ast_ty(&ty.elem);
					fargs.push(ty.clone());
					nargs.push(ty);
				}

				let ret = ret.map_or(Type::Void, |t| convert_ast_ty(&t));

				let ty = Type::Fn(nargs, Box::new(ret.clone()));
				self.peek_scope_mut().locals.push((id, name.elem.to_string(), ty));

				Node::FuncDecl {
					id, ret,
					args: fargs,
				}
			},
			ast::Node::Func { name, args, ret, attrs, body } => {
				let id = self.peek_scope_mut().new_id();
				self.push_new_scope();

				let mut nargs = Vec::new();
				let mut fargs = Vec::new();
				for (n, ty) in args {
					if matches!(*ty, ast::Type::Void) {
						return ReportKind::TypeError
							.title("Type 'void' is not allowed as a function argument")
							.help("Remove the arg, or change the type to '*void'")
							.span(ty.span)
							.as_err();
					}

					let ty = convert_ast_ty(&ty.elem);

					let id = self.peek_scope_mut().new_id();
					self.peek_scope_mut().locals.push((id, n.elem.to_string(), ty.clone()));
					fargs.push((id, ty.clone()));
					nargs.push(ty);
				}

				let ret = ret.map_or(Type::Void, |t| convert_ast_ty(&t));
				let ty = Type::Fn(nargs, Box::new(ret.clone()));

				let scope_len = self.scope.len();
				self.scope.get_mut(scope_len - 2).unwrap()
					.locals.push((id, name.elem.to_string(), ty));

				// TODO: try fold? 🥺👉👈
				let mut nbody = Vec::new();
				for node in body {
					nbody.extend(self.analyze_stmt(node, &ret)?);
				}

				Node::Func {
					id, ret, 
					body:   nbody,
					args:   fargs,
					export: attrs.iter().any(|a| matches!(**a, ast::Attrs::Export)),
				}
			},
			_ => todo!(),
		})
	}

	fn analyze_stmt(&mut self, node: Sp<ast::Node>, ret: &Type) -> Result<Vec<Node>> {
		Ok(match node.elem {
			ast::Node::Ret(None) => vec![Node::Ret(None, Type::Void)],
			ast::Node::Ret(Some(node)) => {
				let (ty, n, v) = self.analyze_expr(*node)?;

				let mut nodes = n.map_or(Vec::new(), |n| vec![n]);

				nodes.push(Node::Ret(Some(v), ty));
				nodes
			},
			ast::Node::FuncCall { name, args } => {
				// TODO: do the whole overloading match thing
				let (depth, (id, _, ty)) = self.find_matching_descending(|(_, n, _)| n == name.elem)
					.ok_or_else(|| ReportKind::Undefined
						.title(format!("Function '{}' is not defined", *name))
						.span(name.span))?;

				let Type::Fn(fn_args, fn_ret) = ty else {
					return ReportKind::TypeError
						.title(format!("'{}' is not callable", *name))
						.help("consider changing the type to 'fn(...) ...'")
						.span(name.span)
						.as_err();
				};

				let id = match depth {
					0 => Var::Glob(*id),
					_ => Var::Local(*id),
				};

				let mut nargs = Vec::new();
				let mut nodes = Vec::new();
				for arg in args {
					let (t, n, v) = self.analyze_expr(arg)?;

					// TODO: type check that args match
					n.map(|n| nodes.push(n));
					nargs.push((v, t));
				}

				nodes.push(Node::FuncCall { id, args: nargs });
				nodes
			},
			_ => todo!(),
		})
	}

	fn analyze_expr(&mut self, node: Sp<ast::Node>) -> Result<(Type, Option<Node>, Var)> {
		Ok(match node.elem {
			ast::Node::StrLit(s) => {
				let id = self.get_global_mut().new_id();

				let ty = Type::Arr(Type::U(8).into(), Some(s.len() as u64));
				
				self.get_global_mut().locals.push((id, format!("__const{id:?}"), ty.clone()));
				(ty.clone(), Some(Node::Global { 
					ty, id, 
					val: Node::StrLit(s).into(),
				}), Var::Glob(id))
			},
			// TODO: add placeholder type for ints
			ast::Node::UIntLit(v) => (Type::Usize, None, Var::Imm(v)),
			_ => todo!(),
		})
	}
}

fn convert_ast_ty(ty: &ast::Type) -> Type {
	match ty {
		ast::Type::U(n)  => Type::U(*n),
		ast::Type::I(n)  => Type::I(*n),
		ast::Type::B(n)  => Type::B(*n),
		ast::Type::F(n)  => Type::F(*n),
		ast::Type::Usize => Type::Usize,
		ast::Type::Isize => Type::Isize,
		ast::Type::Void  => Type::Void,
		ast::Type::Never => Type::Never,
		ast::Type::Ptr(ty)    => Type::Ptr(convert_ast_ty(ty).into()),
		ast::Type::Arr(ty, n) => Type::Arr(convert_ast_ty(ty).into(), *n),
		ast::Type::Mut(ty)    => Type::Mut(convert_ast_ty(ty).into()),
		ast::Type::Opt(ty)    => Type::Opt(convert_ast_ty(&ty).into()),
		ast::Type::Fn(args, ret) => Type::Fn(
			args.iter().map(|t| convert_ast_ty(&t)).collect(),
			Box::new(ret.as_ref().map_or(Type::Void, |t| convert_ast_ty(t)))),
		ast::Type::Ident(_) => unimplemented!("ident"),
	}
}
