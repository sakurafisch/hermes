# Copyright (c) Facebook, Inc. and its affiliates.
#
# This source code is licensed under the MIT license found in the
# LICENSE file in the root directory of this source tree.

set(HERMES_LINK_COMPONENTS LLVHSupport)

add_hermes_tool(hbc-attribute
  hbc-attribute.cpp
  ${ALL_HEADER_FILES}
  )

target_link_libraries(hbc-attribute
  hermesHBCBackend
)
