cbuffer Parameters : register(b0) { float frame; float width; float height; float spare; }
float4 vertex(uint id : SV_VertexID) : SV_POSITION {
    return float4(float2((id << 1) & 2, id & 2) * float2(2, -2) + float2(-1, 1), 0, 1);
}
float4 pixel(float4 pos : SV_POSITION) : SV_TARGET {
    // Moving text-like edges, colored panels and textured gradients. Deterministic
    // and generated entirely on the GPU; no desktop pixels or CPU frame uploads.
    float2 p = pos.xy + float2(frame * 3, frame);
    float2 cell = floor(p / 16);
    float grid = step(2, fmod(p.x, 16)) * step(2, fmod(p.y, 16));
    float shade = frac(sin(dot(cell, float2(12.9898, 78.233))) * 43758.5453);
    float3 color = float3(0.1 + shade * 0.6, 0.1 + p.x / width * 0.6, 0.2 + p.y / height * 0.5);
    color *= lerp(0.1, 1, grid);
    if (fmod(pos.x + frame * 8, width) < 128) color = float3(1.5, 1.3, 0.7);
    return float4(color * 3, 1); // 240-nit scRGB white, with HDR highlights.
}
