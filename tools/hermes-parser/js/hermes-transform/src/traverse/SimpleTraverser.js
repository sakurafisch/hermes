/**
 * Copyright (c) Facebook, Inc. and its affiliates.
 *
 * This source code is licensed under the MIT license found in the
 * LICENSE file in the root directory of this source tree.
 *
 * @flow strict-local
 * @format
 */

'use strict';

import type {ESNode} from 'hermes-estree';

import {VisitorKeys} from 'hermes-eslint';

/**
 * Check whether the given value is an ASTNode or not.
 * @param x The value to check.
 * @returns `true` if the value is an ASTNode.
 */
function isNode(x: mixed): boolean {
  return x !== null && typeof x === 'object' && typeof x.type === 'string';
}

export type TraverserCallback = (node: ESNode, parent: ?ESNode) => void;
export type TraverserOptions = $ReadOnly<{
  /** The callback function which is called on entering each node. */
  enter: TraverserCallback,
  /** The callback function which is called on leaving each node. */
  leave: TraverserCallback,
}>;

/**
 * Can be thrown within the traversal "enter" function to prevent the traverser
 * from traversing the node any further, essentially culling the remainder of the
 * AST branch
 */
export const SimpleTraverserSkip: Error = new Error();
/**
 * Can be thrown at any point during the traversal to immediately stop traversal
 * entirely.
 */
export const SimpleTraverserBreak: Error = new Error();

/**
 * A very simple traverser class to traverse AST trees.
 */
export class SimpleTraverser {
  /**
   * Traverse the given AST tree.
   * @param node The root node to traverse.
   * @param options The option object.
   */
  traverse(node: ESNode, options: TraverserOptions): void {
    try {
      this._traverse(node, null, options);
    } catch (ex) {
      if (ex === SimpleTraverserBreak) {
        return;
      }
      throw ex;
    }
  }

  /**
   * Traverse the given AST tree recursively.
   * @param node The current node.
   * @param parent The parent node.
   * @private
   */
  _traverse(node: ESNode, parent: ?ESNode, options: TraverserOptions): void {
    if (!isNode(node)) {
      return;
    }

    try {
      options.enter(node, parent);
    } catch (ex) {
      if (ex === SimpleTraverserSkip) {
        return;
      }
      throw ex;
    }

    const keys = VisitorKeys[node.type];
    if (keys == null) {
      throw new Error(`No visitor keys found for node type "${node.type}".`);
    }

    if (keys.length >= 1) {
      for (let i = 0; i < keys.length; ++i) {
        // $FlowExpectedError[prop-missing]
        const child = node[keys[i]];

        if (Array.isArray(child)) {
          for (let j = 0; j < child.length; ++j) {
            this._traverse(child[j], node, options);
          }
        } else {
          this._traverse(child, node, options);
        }
      }
    }

    try {
      options.leave(node, parent);
    } catch (ex) {
      if (ex === SimpleTraverserSkip) {
        return;
      }
      throw ex;
    }
  }

  /**
   * Traverse the given AST tree.
   * @param node The root node to traverse.
   * @param options The option object.
   */
  static traverse(node: ESNode, options: TraverserOptions) {
    new SimpleTraverser().traverse(node, options);
  }
}
