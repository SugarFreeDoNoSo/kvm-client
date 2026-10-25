#ifdef GL_ES
precision mediump float;
#endif
varying vec2 v_texcoord;
uniform sampler2D tex;
uniform float width;
uniform float height;
uniform float amount;

// Spatial unsharp mask: no previous frames and no frame-generation latency.
void main() {
    vec2 step_size = vec2(1.0 / max(width, 1.0), 1.0 / max(height, 1.0));
    vec4 center = texture2D(tex, v_texcoord);
    vec3 north = texture2D(tex, v_texcoord + vec2(0.0, step_size.y)).rgb;
    vec3 south = texture2D(tex, v_texcoord - vec2(0.0, step_size.y)).rgb;
    vec3 east = texture2D(tex, v_texcoord + vec2(step_size.x, 0.0)).rgb;
    vec3 west = texture2D(tex, v_texcoord - vec2(step_size.x, 0.0)).rgb;
    vec3 result = center.rgb + amount * (center.rgb - (north + south + east + west) * 0.25);
    // Clamp to the local range to avoid ringing around desktop text.
    vec3 low = min(center.rgb, min(min(north, south), min(east, west)));
    vec3 high = max(center.rgb, max(max(north, south), max(east, west)));
    gl_FragColor = vec4(clamp(result, low, high), center.a);
}
