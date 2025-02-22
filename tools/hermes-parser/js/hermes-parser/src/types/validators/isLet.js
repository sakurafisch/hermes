/**
 * Copyright (c) Facebook, Inc. and its affiliates.
 *
 * This source code is licensed under the MIT license found in the
 * LICENSE file in the root directory of this source tree.
 *
 * @format
 */

import {isVariableDeclaration} from '../generated/node-types';
import {BLOCK_SCOPED_SYMBOL} from '../definitions/constants';

/**
 * Check if the input `node` is a `let` variable declaration.
 */
export default function isLet(node: t.Node): boolean {
  return (
    isVariableDeclaration(node) &&
    (node.kind !== 'var' || node[BLOCK_SCOPED_SYMBOL])
  );
}
