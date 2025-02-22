/*
 * Copyright (c) Facebook, Inc. and its affiliates.
 *
 * This source code is licensed under the MIT license found in the
 * LICENSE file in the root directory of this source tree.
 */

use juno::ast::*;

mod validate;

#[test]
#[should_panic]
fn test_node_outlives_context() {
    let ast;
    {
        let mut ctx = Context::new();
        ast = {
            let gc = GCContext::new(&mut ctx);
            NodePtr::from_node(
                &gc,
                NumericLiteralBuilder::build_template(
                    &gc,
                    NumericLiteralTemplate {
                        metadata: Default::default(),
                        value: 1.0f64,
                    },
                ),
            )
        };
        // Forget `ast` in order to prevent the `Drop` impl from being called on panic.
        std::mem::forget(ast);
    }
}

#[test]
#[should_panic]
#[allow(clippy::redundant_clone)]
fn test_node_clone_outlives_context() {
    let ast;
    {
        let mut ctx = Context::new();
        {
            let ast_orig = {
                let gc = GCContext::new(&mut ctx);
                NodePtr::from_node(
                    &gc,
                    NumericLiteralBuilder::build_template(
                        &gc,
                        NumericLiteralTemplate {
                            metadata: Default::default(),
                            value: 1.0f64,
                        },
                    ),
                )
            };
            ast = ast_orig.clone();
            // Forget `ast` in order to prevent the `Drop` impl from being called on panic.
            std::mem::forget(ast);
        }
    }
}

#[test]
#[allow(clippy::float_cmp)]
fn test_visit() {
    let mut ctx = Context::new();
    let ast = {
        let gc = GCContext::new(&mut ctx);
        NodePtr::from_node(
            &gc,
            BlockStatementBuilder::build_template(
                &gc,
                BlockStatementTemplate {
                    metadata: Default::default(),
                    body: vec![
                        ExpressionStatementBuilder::build_template(
                            &gc,
                            ExpressionStatementTemplate {
                                metadata: Default::default(),
                                expression: NumericLiteralBuilder::build_template(
                                    &gc,
                                    NumericLiteralTemplate {
                                        metadata: Default::default(),
                                        value: 1.0,
                                    },
                                ),
                                directive: None,
                            },
                        ),
                        ExpressionStatementBuilder::build_template(
                            &gc,
                            ExpressionStatementTemplate {
                                metadata: Default::default(),
                                expression: NumericLiteralBuilder::build_template(
                                    &gc,
                                    NumericLiteralTemplate {
                                        metadata: Default::default(),
                                        value: 2.0,
                                    },
                                ),
                                directive: None,
                            },
                        ),
                    ],
                },
            ),
        )
    };

    // Accumulates the numbers found in the AST.
    struct NumberFinder {
        acc: Vec<f64>,
    }

    impl<'gc> Visitor<'gc> for NumberFinder {
        /// Visit the Node `node` with the given `parent`.
        fn call(
            &mut self,
            ctx: &'gc GCContext,
            node: &'gc Node<'gc>,
            parent: Option<&'gc Node<'gc>>,
        ) {
            if let Node::NumericLiteral(NumericLiteral { value, .. }) = node {
                assert!(matches!(
                    parent.unwrap(),
                    Node::ExpressionStatement(ExpressionStatement { .. })
                ));
                self.acc.push(*value);
            }
            node.visit_children(ctx, self);
        }
    }

    let mut visitor = NumberFinder { acc: vec![] };
    let gc = GCContext::new(&mut ctx);
    ast.node(&gc).visit(&gc, &mut visitor, None);
    assert_eq!(visitor.acc, [1.0, 2.0]);
}

#[test]
fn test_visit_mut() {
    let mut ctx = Context::new();

    let (x, y) = (1.0, 2.0);
    let ast = {
        let gc = GCContext::new(&mut ctx);
        NodePtr::from_node(
            &gc,
            ExpressionStatementBuilder::build_template(
                &gc,
                ExpressionStatementTemplate {
                    metadata: Default::default(),
                    directive: None,
                    expression: BinaryExpressionBuilder::build_template(
                        &gc,
                        BinaryExpressionTemplate {
                            metadata: Default::default(),
                            operator: BinaryExpressionOperator::Plus,
                            left: NumericLiteralBuilder::build_template(
                                &gc,
                                NumericLiteralTemplate {
                                    metadata: Default::default(),
                                    value: x,
                                },
                            ),
                            right: UnaryExpressionBuilder::build_template(
                                &gc,
                                UnaryExpressionTemplate {
                                    metadata: Default::default(),
                                    prefix: true,
                                    operator: UnaryExpressionOperator::Minus,
                                    argument: NumericLiteralBuilder::build_template(
                                        &gc,
                                        NumericLiteralTemplate {
                                            metadata: Default::default(),
                                            value: y,
                                        },
                                    ),
                                },
                            ),
                        },
                    ),
                },
            ),
        )
    };

    struct Pass {}

    impl<'gc> VisitorMut<'gc> for Pass {
        fn call(
            &mut self,
            ctx: &'gc GCContext,
            node: &'gc Node<'gc>,
            _parent: Option<&'gc Node<'gc>>,
        ) -> TransformResult<&'gc Node<'gc>> {
            if let Node::BinaryExpression(
                e1 @ BinaryExpression {
                    operator: BinaryExpressionOperator::Plus,
                    right:
                        Node::UnaryExpression(UnaryExpression {
                            operator: UnaryExpressionOperator::Minus,
                            argument: e2,
                            ..
                        }),
                    ..
                },
            ) = node
            {
                let mut builder = BinaryExpressionBuilder::from_node(e1);
                builder.operator(BinaryExpressionOperator::Minus);
                builder.right(e2);
                return node.visit_children_mut(NodeBuilder::BinaryExpression(builder), ctx, self);
            }
            node.visit_children_mut(NodeBuilder::from_node(node), ctx, self)
        }
    }
    let mut pass = Pass {};

    let transformed = {
        let gc = GCContext::new(&mut ctx);
        NodePtr::from_node(&gc, ast.node(&gc).visit_mut(&gc, &mut pass, None))
    };

    {
        let gc = GCContext::new(&mut ctx);
        match transformed.node(&gc) {
            Node::ExpressionStatement(ExpressionStatement {
                expression:
                    Node::BinaryExpression(BinaryExpression {
                        operator: BinaryExpressionOperator::Minus,
                        left:
                            Node::NumericLiteral(NumericLiteral {
                                value: val_left, ..
                            }),
                        right:
                            Node::NumericLiteral(NumericLiteral {
                                value: val_right, ..
                            }),
                        ..
                    }),
                ..
            }) => {
                assert_eq!(
                    (*val_left, *val_right),
                    (x, y),
                    "Transformation failed: {:#?}",
                    transformed.node(&gc),
                );
            }
            _ => panic!("Transformation failed: {:#?}", transformed.node(&gc)),
        };
    }

    {
        ctx.gc();
    }
}

#[test]
fn test_many_nodes() {
    let mut ctx = Context::new();
    let mut cached = None;
    let mut val = 0f64;
    for _ in 0..10 {
        {
            let gc = GCContext::new(&mut ctx);
            for i in 0..10_000 {
                cached = Some(NodePtr::from_node(
                    &gc,
                    NumericLiteralBuilder::build_template(
                        &gc,
                        NumericLiteralTemplate {
                            metadata: Default::default(),
                            value: i as f64,
                        },
                    ),
                ));
                val = i as f64;
            }
        }
        ctx.gc();
    }

    let gc = GCContext::new(&mut ctx);
    match cached.unwrap().node(&gc) {
        Node::NumericLiteral(NumericLiteral { value, .. }) => {
            assert!(
                (*value - val).abs() < f64::EPSILON,
                "Incorrect cached value: {:#?}",
                *value
            );
        }
        n => {
            panic!("Incorrect cached value: {:#?}", n);
        }
    };
}

#[test]
fn test_store_node() {
    let mut ctx = Context::new();

    struct Foo<'gc> {
        n: Option<&'gc Node<'gc>>,
    }

    impl<'gc> Foo<'gc> {
        fn set_n(&mut self, node: &'gc Node<'gc>) {
            self.n = Some(node);
        }
    }

    impl<'gc> Visitor<'gc> for Foo<'gc> {
        fn call(
            &mut self,
            gc: &'gc GCContext,
            node: &'gc Node<'gc>,
            _parent: Option<&'gc Node<'gc>>,
        ) {
            self.set_n(node);
            node.visit_children(gc, self)
        }
    }

    {
        let gc = GCContext::new(&mut ctx);
        let mut pass = Foo { n: None };
        let ast = NodePtr::from_node(
            &gc,
            NumericLiteralBuilder::build_template(
                &gc,
                NumericLiteralTemplate {
                    metadata: Default::default(),
                    value: 1.0f64,
                },
            ),
        );
        ast.node(&gc).visit(&gc, &mut pass, None);
        assert!(pass.n.is_some());
    }
}
