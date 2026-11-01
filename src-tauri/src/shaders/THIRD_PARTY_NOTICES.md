# Shader attribution

`gaming.frag` and `gaming-core.frag` adapt Snapdragon Game Super Resolution v1,
[sgsr1_shader_mobile.frag](https://github.com/SnapdragonGameStudios/snapdragon-gsr/blob/main/sgsr/v1/include/glsl/sgsr1_shader_mobile.frag).

The shader reference carries copyright (c) 2025 Qualcomm Innovation Center, Inc.
The repository's license below carries its original 2023 copyright.
Changes: GStreamer uniform and varying names; separate input/output sizes;
GLSL 1.20-compatible and GLSL 1.50 interfaces; explicit texel-center samples
replacing texture gathers with the same 12-tap support; clamp-to-edge sampling;
centered manual zoom with integer-pixel origin; defensive near-zero division
guards; and `amount` mapping to SGSR EdgeSharpness in its documented [1,2] range.
Green-channel edge mode, Lanczos-like weights, adaptive contrast weighting,
local clamp and correction limit 23/255 follow the reference. `edge_threshold`
exposes the edge-vote cutoff (default 8/255, configurable from 1/255 to 32/255).

When fitting a larger source to a smaller viewport with `downscale_aa` enabled,
a separate area-sampling path averages the destination pixel's source footprint
with up to 8x8 equal-area strata and bypasses SGSR sharpening. Above 8:1 reduction
the grid remains bounded and approximates the full footprint. This fallback and
the configurable edge cutoff are project additions, not reference SGSR behavior.
The port uses no temporal history or generated frames. Desktop, original and
nearest-neighbor shaders are project implementations and do not derive from SGSR.

## Snapdragon GSR license (BSD-3-Clause)

Copyright (c) 2023, Qualcomm Innovation Center, Inc. All rights reserved.

Redistribution and use in source and binary forms, with or without
modification, are permitted provided that the following conditions are met:

1. Redistributions of source code must retain the above copyright notice,
   this list of conditions and the following disclaimer.
2. Redistributions in binary form must reproduce the above copyright notice,
   this list of conditions and the following disclaimer in the documentation
   and/or other materials provided with the distribution.
3. Neither the name of the copyright holder nor the names of its contributors
   may be used to endorse or promote products derived from this software
   without specific prior written permission.

THIS SOFTWARE IS PROVIDED BY THE COPYRIGHT HOLDERS AND CONTRIBUTORS "AS IS"
AND ANY EXPRESS OR IMPLIED WARRANTIES, INCLUDING, BUT NOT LIMITED TO, THE
IMPLIED WARRANTIES OF MERCHANTABILITY AND FITNESS FOR A PARTICULAR PURPOSE
ARE DISCLAIMED. IN NO EVENT SHALL THE COPYRIGHT HOLDER OR CONTRIBUTORS BE
LIABLE FOR ANY DIRECT, INDIRECT, INCIDENTAL, SPECIAL, EXEMPLARY, OR
CONSEQUENTIAL DAMAGES (INCLUDING, BUT NOT LIMITED TO, PROCUREMENT OF
SUBSTITUTE GOODS OR SERVICES; LOSS OF USE, DATA, OR PROFITS; OR BUSINESS
INTERRUPTION) HOWEVER CAUSED AND ON ANY THEORY OF LIABILITY, WHETHER IN
CONTRACT, STRICT LIABILITY, OR TORT (INCLUDING NEGLIGENCE OR OTHERWISE)
ARISING IN ANY WAY OUT OF THE USE OF THIS SOFTWARE, EVEN IF ADVISED OF THE
POSSIBILITY OF SUCH DAMAGE.

SPDX-License-Identifier: BSD-3-Clause
