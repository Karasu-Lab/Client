layout(set = 2, binding = 0) uniform ItemTintPalette {
    uvec4 item_tints[4096];
};

vec4 unpack_argb(uint color) {
    return vec4(
        float((color >> 16) & 255u),
        float((color >> 8) & 255u),
        float(color & 255u),
        float((color >> 24) & 255u)
    ) / 255.0;
}

vec4 resolve_item_tint(uint tint_index, uint base, uint count) {
    uint index = base + tint_index;
    if (tint_index >= count || index >= 16384u) {
        return vec4(1.0);
    }
    return unpack_argb(item_tints[index >> 2u][index & 3u]);
}
