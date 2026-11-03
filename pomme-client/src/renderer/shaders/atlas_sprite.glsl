layout(set = 1, binding = 0) uniform texture2D atlas_texture;
layout(set = 1, binding = 1) uniform sampler atlas_sampler;
layout(set = 1, binding = 2) uniform SpriteRects {
    uvec4 sprite_rects[4096];
};

vec4 sample_atlas_sprite(vec2 sprite_uv, uint sprite) {
    uvec4 atlas_rect = sprite_rects[sprite];
    vec2 atlas_size = vec2(textureSize(sampler2D(atlas_texture, atlas_sampler), 0));
    vec2 atlas_uv = (vec2(atlas_rect.xy) + sprite_uv * vec2(atlas_rect.zw)) / atlas_size;
    return texture(sampler2D(atlas_texture, atlas_sampler), atlas_uv);
}
