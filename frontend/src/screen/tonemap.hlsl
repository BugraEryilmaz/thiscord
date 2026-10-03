// WGC FP16 is linear scRGB, where 1.0 represents 80 nits. Normalize the
// monitor's SDR reference white before mapping highlights into SDR BT.709.
Texture2D<float4> source : register(t0);
SamplerState linearClamp : register(s0);
cbuffer Parameters : register(b0) { float whiteScale; float hdr; float2 padding; };
struct Vertex { float4 position : SV_Position; float2 uv : TEXCOORD0; };
Vertex vertex(uint id : SV_VertexID) {
    Vertex o;
    o.uv = float2((id << 1) & 2, id & 2);
    o.position = float4(o.uv * float2(2, -2) + float2(-1, 1), 0, 1);
    return o;
}
float transfer(float x) { return x < 0.018 ? 4.5 * x : 1.099 * pow(x, 0.45) - 0.099; }
float4 pixel(Vertex v) : SV_Target {
    float3 c = max(source.SampleLevel(linearClamp, v.uv, 0).rgb / whiteScale, 0);
    // Luminance-preserving shoulder: SDR mids are unchanged, HDR highlights
    // roll off above 75% reference white. Clamp out-of-SDR-gamut components.
    float l = dot(c, float3(0.2126, 0.7152, 0.0722));
    if (hdr > 0.5 && l > 0.75) {
        float mapped = 0.75 + 0.25 * (1 - exp(-(l - 0.75) / 0.25));
        c *= mapped / l;
    }
    c = saturate(c);
    return float4(transfer(c.r), transfer(c.g), transfer(c.b), 1);
}
