#ifdef GL_ES
precision highp float;
#endif
varying vec2 v_texcoord;
uniform sampler2D tex;
uniform float input_width;
uniform float input_height;
uniform float output_width;
uniform float output_height;
uniform float native_size;
uniform float amount;
uniform float zoom;
uniform float anti_ringing;
uniform float edge_threshold;
uniform float downscale_aa;

vec2 source_size() {
    return max(vec2(input_width, input_height), vec2(1.0));
}

vec2 viewport_size() {
    return max(vec2(output_width, output_height), vec2(1.0));
}

vec2 source_uv() {
    if (native_size > 0.5) {
        vec2 content_size = source_size() * clamp(zoom, 1.0, 4.0);
        // Integer origin preserves pixel boundaries at integer zoom, including
        // odd viewport differences; oversized content is cropped symmetrically.
        vec2 origin = floor((viewport_size() - content_size) * 0.5);
        return (v_texcoord * viewport_size() - origin) / content_size;
    }
    return v_texcoord;
}

vec4 sample_source(vec2 uv) {
    // Clamp to texel centers so black borders never bleed into image edges.
    vec2 half_pixel = vec2(0.5) / source_size();
    return texture2D(tex, clamp(uv, half_pixel, vec2(1.0) - half_pixel));
}

void main() {
    vec2 uv = source_uv();
    if (any(lessThan(uv, vec2(0.0))) || any(greaterThan(uv, vec2(1.0)))) {
        gl_FragColor = vec4(0.0, 0.0, 0.0, 1.0);
        return;
    }
    gl_FragColor = sample_source(uv);
}
