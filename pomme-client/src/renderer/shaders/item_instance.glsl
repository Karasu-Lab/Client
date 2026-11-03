// The item pipelines' per-instance data (binding 1), as item_entity.rs's
// `ItemInstance` lays it out.
// WGSL vertex inputs cannot be matrices, so each is its columns at consecutive
// locations, which is how the attributes are laid out either way.
layout(location = 5) in vec4 model_c0;
layout(location = 6) in vec4 model_c1;
layout(location = 7) in vec4 model_c2;
layout(location = 8) in vec4 model_c3;
layout(location = 9) in vec3 normal_matrix_c0;
layout(location = 10) in vec3 normal_matrix_c1;
layout(location = 11) in vec3 normal_matrix_c2;
mat4 instance_model() {
    return mat4(model_c0, model_c1, model_c2, model_c3);
}
mat3 instance_normal_matrix() {
    return mat3(normal_matrix_c0, normal_matrix_c1, normal_matrix_c2);
}
layout(location = 12) in float instance_light;
layout(location = 13) in uint item_tint_base;
layout(location = 14) in uint item_tint_count;
layout(location = 15) in uint outline_color;
