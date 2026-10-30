// GPU-only D3D12 interpolation bridge. GStreamer owns presentation and audio.
// Decoded pixels are never mapped. The only readback is 16 bytes of GPU timing.
#include <gst/gst.h>
#include <gst/video/video.h>
#include <gst/d3d12/gstd3d12.h>
#include <d3dcompiler.h>
#include <wrl/client.h>
#include <array>
#include <vector>
#include <string>
#include <stdexcept>
#include <cstring>
#include <algorithm>
#include <memory>
#include <mutex>
#include "mochune_interpolation_shaders.h"
#include "mochune_interpolation_cache.h"
using Microsoft::WRL::ComPtr;
static void check(HRESULT hr,const char *what) {
  if(FAILED(hr)) throw std::runtime_error(std::string(what)+" (HRESULT "+std::to_string((unsigned)hr)+")");
}
static void error_text(char *dst,size_t cap,const char *text) {
  if(cap){strncpy_s(dst,cap,text,_TRUNCATE);}
}
static const char *compat=R"HLSL(
cbuffer Params:register(b0){float2 image_size;float fraction;float unused;float2 output_size;float2 padding;}
Texture2D<float4> previous_frame:register(t0),next_frame:register(t1),motion_field:register(t2),seed_field:register(t3),previous_preview:register(t4),next_preview:register(t5),camera_field:register(t6);
RWTexture2D<float4> output_image:register(u0);
SamplerState linear_clamp:register(s0);
static float2 uv;static float4 result;
float2 v2(float a){return a.xx;} float2 v2(float2 a){return a;} float2 v2(int2 a){return (float2)a;} float2 v2(float a,float b){return float2(a,b);}
float3 v3(float a){return a.xxx;} float3 v3(float3 a){return a;} float3 v3(float a,float b,float c){return float3(a,b,c);}
float4 v4(float a){return a.xxxx;} float4 v4(float4 a){return a;} float4 v4(float3 a,float b){return float4(a,b);} float4 v4(float2 a,float2 b){return float4(a,b);} float4 v4(float a,float b,float c,float d){return float4(a,b,c,d);}
int2 i2(int a){return a.xx;} int2 i2(int2 a){return a;} int2 i2(float2 a){return (int2)a;} int2 i2(int a,int b){return int2(a,b);}
float4 sample_tex(Texture2D<float4> t,float2 p){return t.SampleLevel(linear_clamp,p,0);}
int2 textureSize(Texture2D<float4> t,int level){uint w,h;t.GetDimensions(w,h);return int2(w,h);}
float4 texelFetch(Texture2D<float4> t,int2 p,int level){return t.Load(int3(p,level));}
#define vec2 v2
#define vec3 v3
#define vec4 v4
#define ivec2 i2
#define mix lerp
#define fract frac
#define greaterThanEqual(a,b) ((a)>=(b))
#define lessThanEqual(a,b) ((a)<=(b))
#define greaterThan(a,b) ((a)>(b))
#define lessThan(a,b) ((a)<(b))
)HLSL";
// Constructor macros are undefined for declarations: translate their type uses
// separately at build time, while constructor calls remain compatibility funcs.
static ComPtr<ID3DBlob> compile_shader(const char *body) {
  std::string src=compat;
  // GLSL uses the same spelling for types and constructors; preserve only the
  // constructors as helpers so scalar splats retain their original semantics.
  std::string code=body;
  {size_t pos=0;while((pos=code.find("texture(",pos))!=std::string::npos){code.replace(pos,8,"sample_tex(");pos+=11;}}
  {size_t pos=0;while((pos=code.find("vector",pos))!=std::string::npos){code.replace(pos,6,"flow_vector");pos+=11;}}
  for(auto pair: {std::pair<const char*,const char*>("ivec2 ","int2 "),{"vec2 ","float2 "},{"vec3 ","float3 "},{"vec4 ","float4 "}}){
    size_t pos=0;while((pos=code.find(pair.first,pos))!=std::string::npos){code.replace(pos,strlen(pair.first),pair.second);pos+=strlen(pair.second);}
  }
  // Array declarations and parameter types also use the type replacement above.
  src+=code;
  src+=R"HLSL(
[numthreads(8,8,1)] void main(uint3 id:SV_DispatchThreadID){
 if(any(id.xy>=(uint2)output_size))return;
 uv=(float2(id.xy)+0.5)/output_size;source_main();output_image[id.xy]=result;
})HLSL";
  uint64_t hash=14695981039346656037ull;for(unsigned char c:src){hash^=c;hash*=1099511628211ull;}
  for(const auto &cached:cached_shaders)if(cached.hash==hash){
    ComPtr<ID3DBlob> blob;check(D3DCreateBlob(cached.size,&blob),"Load interpolation shader bytecode");
    memcpy(blob->GetBufferPointer(),cached.data,cached.size);return blob;
  }
  ComPtr<ID3DBlob> blob,err;
  HRESULT hr=D3DCompile(src.data(),src.size(),"Mochune motion interpolation",nullptr,nullptr,"main","cs_5_0",D3DCOMPILE_OPTIMIZATION_LEVEL2|D3DCOMPILE_PREFER_FLOW_CONTROL,0,&blob,&err);
  if(FAILED(hr))throw std::runtime_error(err?std::string((char*)err->GetBufferPointer(),err->GetBufferSize()):"HLSL compiler failed");
  return blob;
}
static const char *bodies[]={shader_motion,shader_refine,shader_seed,shader_warp,shader_preview,shader_consensus};
static ComPtr<ID3DBlob> compiled_shader(int i){
  static std::array<ComPtr<ID3DBlob>,6> shaders;static std::once_flag once;
  std::call_once(once,[]{for(int n=0;n<6;n++)shaders[n]=compile_shader(bodies[n]);});
  return shaders[i];
}
struct Texture {ComPtr<ID3D12Resource> resource; D3D12_RESOURCE_STATES state=D3D12_RESOURCE_STATE_COMMON;};
struct Gpu {
  std::shared_ptr<GstD3D12Device> gst_owner; std::shared_ptr<void> event_owner;
  ComPtr<ID3D12Device> device; GstD3D12Device *gst_device=nullptr;
  ComPtr<ID3D12CommandQueue> queue;ComPtr<ID3D12CommandAllocator> allocator;
  ComPtr<ID3D12GraphicsCommandList> list;ComPtr<ID3D12Fence> fence;
  ComPtr<ID3D12RootSignature> root;ComPtr<ID3D12DescriptorHeap> heap;
  std::array<ComPtr<ID3D12PipelineState>,6> pso;
  ComPtr<ID3D12QueryHeap> query;ComPtr<ID3D12Resource> timing;
  std::array<Texture,8> textures; // pair A/B, raw/refined/seed, preview A/B, camera
  std::vector<ComPtr<ID3D12Resource>> retained;
  std::array<GstBuffer*,2> inputs{nullptr,nullptr};
  UINT width,height,grid_w,grid_h,stride=0,descriptor=0;
  UINT64 serial=0,frequency=0,last_ns=0;HANDLE event=nullptr;
  bool submitted=false;
  Gpu(GstBuffer *sample,UINT w,UINT h):width(w),height(h),grid_w((w+7)/8),grid_h((h+7)/8) {
    auto mem=gst_buffer_peek_memory(sample,0);
    if(!gst_is_d3d12_memory(mem))throw std::runtime_error("Interpolation requires D3D12 GPU memory");
    auto m=GST_D3D12_MEMORY_CAST(mem);gst_device=(GstD3D12Device*)gst_object_ref(m->device);
    gst_owner=std::shared_ptr<GstD3D12Device>(gst_device,[](GstD3D12Device*p){gst_object_unref(p);});
    device=gst_d3d12_device_get_device_handle(gst_device);
    D3D12_COMMAND_QUEUE_DESC q={};q.Type=D3D12_COMMAND_LIST_TYPE_DIRECT;
    check(device->CreateCommandQueue(&q,IID_PPV_ARGS(&queue)),"Create interpolation queue");
    check(device->CreateCommandAllocator(q.Type,IID_PPV_ARGS(&allocator)),"Create interpolation allocator");
    check(device->CreateCommandList(0,q.Type,allocator.Get(),nullptr,IID_PPV_ARGS(&list)),"Create interpolation commands");list->Close();
    check(device->CreateFence(0,D3D12_FENCE_FLAG_NONE,IID_PPV_ARGS(&fence)),"Create interpolation fence");
    event=CreateEventW(nullptr,FALSE,FALSE,nullptr);if(!event)throw std::runtime_error("Create interpolation completion event");
    event_owner=std::shared_ptr<void>(event,[](void*p){CloseHandle(p);});
    check(queue->GetTimestampFrequency(&frequency),"Get GPU timing frequency");
    D3D12_QUERY_HEAP_DESC query_desc={D3D12_QUERY_HEAP_TYPE_TIMESTAMP,2,0};
    check(device->CreateQueryHeap(&query_desc,IID_PPV_ARGS(&query)),"Create GPU timing query");
    D3D12_HEAP_PROPERTIES hp={};hp.Type=D3D12_HEAP_TYPE_READBACK;
    D3D12_RESOURCE_DESC rd={};rd.Dimension=D3D12_RESOURCE_DIMENSION_BUFFER;rd.Width=16;rd.Height=1;rd.DepthOrArraySize=1;rd.MipLevels=1;rd.SampleDesc.Count=1;rd.Layout=D3D12_TEXTURE_LAYOUT_ROW_MAJOR;
    check(device->CreateCommittedResource(&hp,D3D12_HEAP_FLAG_NONE,&rd,D3D12_RESOURCE_STATE_COPY_DEST,nullptr,IID_PPV_ARGS(&timing)),"Create timing readback");
    D3D12_DESCRIPTOR_HEAP_DESC hd={D3D12_DESCRIPTOR_HEAP_TYPE_CBV_SRV_UAV,128,D3D12_DESCRIPTOR_HEAP_FLAG_SHADER_VISIBLE,0};
    check(device->CreateDescriptorHeap(&hd,IID_PPV_ARGS(&heap)),"Create interpolation descriptors");stride=device->GetDescriptorHandleIncrementSize(hd.Type);
    D3D12_DESCRIPTOR_RANGE ranges[2]={{D3D12_DESCRIPTOR_RANGE_TYPE_SRV,7,0,0,0},{D3D12_DESCRIPTOR_RANGE_TYPE_UAV,1,0,0,0}};
    D3D12_ROOT_PARAMETER params[3]={};for(int i=0;i<2;i++){params[i].ParameterType=D3D12_ROOT_PARAMETER_TYPE_DESCRIPTOR_TABLE;params[i].DescriptorTable={1,&ranges[i]};}
    params[2].ParameterType=D3D12_ROOT_PARAMETER_TYPE_32BIT_CONSTANTS;params[2].Constants={0,0,8};
    D3D12_STATIC_SAMPLER_DESC sampler={};sampler.Filter=D3D12_FILTER_MIN_MAG_MIP_LINEAR;sampler.AddressU=sampler.AddressV=sampler.AddressW=D3D12_TEXTURE_ADDRESS_MODE_CLAMP;sampler.MaxLOD=D3D12_FLOAT32_MAX;sampler.ShaderRegister=0;
    D3D12_ROOT_SIGNATURE_DESC rs={3,params,1,&sampler,D3D12_ROOT_SIGNATURE_FLAG_NONE};ComPtr<ID3DBlob> signature,errors;
    check(D3D12SerializeRootSignature(&rs,D3D_ROOT_SIGNATURE_VERSION_1,&signature,&errors),"Serialize interpolation root signature");
    check(device->CreateRootSignature(0,signature->GetBufferPointer(),signature->GetBufferSize(),IID_PPV_ARGS(&root)),"Create interpolation root signature");
    for(int i=0;i<6;i++){auto shader=compiled_shader(i);D3D12_COMPUTE_PIPELINE_STATE_DESC d={};d.pRootSignature=root.Get();d.CS={shader->GetBufferPointer(),shader->GetBufferSize()};check(device->CreateComputePipelineState(&d,IID_PPV_ARGS(&pso[i])),"Create interpolation shader pipeline");}
    for(int i=0;i<8;i++)textures[i]=texture(i<2?width:i==7?1:grid_w,i<2?height:i==7?1:grid_h,i<2?DXGI_FORMAT_R8G8B8A8_UNORM:DXGI_FORMAT_R16G16B16A16_FLOAT);
  }
  ~Gpu(){for(auto b:inputs)if(b)gst_buffer_unref(b);}
  bool finish(DWORD timeout){if(!submitted)return true;if(fence->GetCompletedValue()<serial){if(FAILED(fence->SetEventOnCompletion(serial,event))||WaitForSingleObject(event,timeout)!=WAIT_OBJECT_0)return false;}
    if(fence->GetCompletedValue()==UINT64_MAX)return false;
    void *data=nullptr;D3D12_RANGE read={0,16};if(SUCCEEDED(timing->Map(0,&read,&data))){auto t=(UINT64*)data;if(t[1]>=t[0]&&frequency)last_ns=(UINT64)((long double)(t[1]-t[0])*1e9L/frequency);D3D12_RANGE written={0,0};timing->Unmap(0,&written);}
    for(auto &b:inputs)if(b){gst_buffer_unref(b);b=nullptr;}retained.clear();submitted=false;return true;
  }
  Texture texture(UINT w,UINT h,DXGI_FORMAT format){Texture t;D3D12_HEAP_PROPERTIES hp={};hp.Type=D3D12_HEAP_TYPE_DEFAULT;
    D3D12_RESOURCE_DESC d={};d.Dimension=D3D12_RESOURCE_DIMENSION_TEXTURE2D;d.Width=w;d.Height=h;d.DepthOrArraySize=1;d.MipLevels=1;d.Format=format;d.SampleDesc.Count=1;d.Flags=D3D12_RESOURCE_FLAG_ALLOW_UNORDERED_ACCESS;
    check(device->CreateCommittedResource(&hp,D3D12_HEAP_FLAG_NONE,&d,D3D12_RESOURCE_STATE_COMMON,nullptr,IID_PPV_ARGS(&t.resource)),"Allocate interpolation GPU texture");return t;}
  void state(Texture &t,D3D12_RESOURCE_STATES next){if(t.state==next)return;D3D12_RESOURCE_BARRIER b={};b.Type=D3D12_RESOURCE_BARRIER_TYPE_TRANSITION;b.Transition={t.resource.Get(),D3D12_RESOURCE_BARRIER_ALL_SUBRESOURCES,t.state,next};list->ResourceBarrier(1,&b);t.state=next;}
  void copy_input(GstBuffer *buffer,int side){auto mem=gst_buffer_peek_memory(buffer,0);if(!gst_is_d3d12_memory(mem))throw std::runtime_error("CPU video frames are not supported");auto m=GST_D3D12_MEMORY_CAST(mem);
    if(m->device!=gst_device)throw std::runtime_error("Interpolation device changed; reload playback");
    auto resource=gst_d3d12_memory_get_resource_handle(m);auto desc=resource->GetDesc();if(desc.Format!=DXGI_FORMAT_R8G8B8A8_UNORM)throw std::runtime_error("Interpolation requires GPU-converted RGBA");
    ComPtr<ID3D12Fence> producer;UINT64 value=0;if(gst_d3d12_memory_get_fence(m,&producer,&value)&&producer)check(queue->Wait(producer.Get(),value),"Wait for decoded GPU frame");
    guint sub=0;if(!gst_d3d12_memory_get_subresource_index(m,0,&sub))throw std::runtime_error("Missing GPU subresource");
    // GStreamer D3D12 external resources are handed off in COMMON state.
    D3D12_RESOURCE_BARRIER b={};b.Type=D3D12_RESOURCE_BARRIER_TYPE_TRANSITION;b.Transition={resource,sub,D3D12_RESOURCE_STATE_COMMON,D3D12_RESOURCE_STATE_COPY_SOURCE};list->ResourceBarrier(1,&b);
    state(textures[side],D3D12_RESOURCE_STATE_COPY_DEST);
    D3D12_TEXTURE_COPY_LOCATION src={},dst={};src.pResource=resource;src.Type=D3D12_TEXTURE_COPY_TYPE_SUBRESOURCE_INDEX;src.SubresourceIndex=sub;dst.pResource=textures[side].resource.Get();dst.Type=D3D12_TEXTURE_COPY_TYPE_SUBRESOURCE_INDEX;
    D3D12_BOX box={0,0,0,width,height,1};list->CopyTextureRegion(&dst,0,0,0,&src,&box);
    std::swap(b.Transition.StateBefore,b.Transition.StateAfter);list->ResourceBarrier(1,&b);state(textures[side],D3D12_RESOURCE_STATE_NON_PIXEL_SHADER_RESOURCE);inputs[side]=gst_buffer_ref(buffer);
  }
  void pass(int shader,Texture &target,UINT w,UINT h,float fraction,int motion){
    std::array<int,7> ids={0,1,motion,4,5,6,7};auto cpu=heap->GetCPUDescriptorHandleForHeapStart();cpu.ptr+=descriptor*stride;auto gpu=heap->GetGPUDescriptorHandleForHeapStart();gpu.ptr+=(UINT64)descriptor*stride;
    for(int i=0;i<7;i++){auto &t=textures[ids[i]];state(t,D3D12_RESOURCE_STATE_NON_PIXEL_SHADER_RESOURCE);D3D12_SHADER_RESOURCE_VIEW_DESC d={};d.Format=t.resource->GetDesc().Format;d.ViewDimension=D3D12_SRV_DIMENSION_TEXTURE2D;d.Shader4ComponentMapping=D3D12_DEFAULT_SHADER_4_COMPONENT_MAPPING;d.Texture2D.MipLevels=1;device->CreateShaderResourceView(t.resource.Get(),&d,cpu);cpu.ptr+=stride;}
    state(target,D3D12_RESOURCE_STATE_UNORDERED_ACCESS);D3D12_UNORDERED_ACCESS_VIEW_DESC u={};u.Format=target.resource->GetDesc().Format;u.ViewDimension=D3D12_UAV_DIMENSION_TEXTURE2D;device->CreateUnorderedAccessView(target.resource.Get(),nullptr,&u,cpu);
    float params[8]={(float)width,(float)height,fraction,0,(float)w,(float)h,0,0};list->SetPipelineState(pso[shader].Get());list->SetComputeRootDescriptorTable(0,gpu);gpu.ptr+=7*stride;list->SetComputeRootDescriptorTable(1,gpu);list->SetComputeRoot32BitConstants(2,8,params,0);list->Dispatch((w+7)/8,(h+7)/8,1);
    D3D12_RESOURCE_BARRIER barrier={};barrier.Type=D3D12_RESOURCE_BARRIER_TYPE_UAV;barrier.UAV.pResource=target.resource.Get();list->ResourceBarrier(1,&barrier);state(target,D3D12_RESOURCE_STATE_NON_PIXEL_SHADER_RESOURCE);descriptor+=8;
  }
  void render(GstBuffer *a,GstBuffer *b,const float *fractions,int count,GstBuffer **outputs){
    if(!finish(2000))throw std::runtime_error("Interpolation GPU timed out");check(allocator->Reset(),"Reset interpolation commands");check(list->Reset(allocator.Get(),nullptr),"Begin interpolation commands");descriptor=0;
    list->SetComputeRootSignature(root.Get());ID3D12DescriptorHeap *heaps[]={heap.Get()};list->SetDescriptorHeaps(1,heaps);list->EndQuery(query.Get(),D3D12_QUERY_TYPE_TIMESTAMP,0);copy_input(a,0);copy_input(b,1);
    pass(4,textures[5],grid_w,grid_h,0,2);pass(4,textures[6],grid_w,grid_h,1,2);
    pass(0,textures[2],grid_w,grid_h,0,2);pass(5,textures[7],1,1,0,2);pass(1,textures[3],grid_w,grid_h,0,2);pass(2,textures[4],grid_w,grid_h,0,3);
    for(int i=0;i<count;i++){
      auto out=texture(width,height,DXGI_FORMAT_R8G8B8A8_UNORM);pass(3,out,width,height,fractions[i],3);state(out,D3D12_RESOURCE_STATE_COMMON);
      auto memory=gst_d3d12_allocator_alloc_wrapped(nullptr,gst_device,out.resource.Get(),0,nullptr,nullptr);if(!memory)throw std::runtime_error("Wrap generated GPU frame");
      outputs[i]=gst_buffer_new();gst_buffer_append_memory(outputs[i],memory);gst_buffer_add_video_meta(outputs[i],GST_VIDEO_FRAME_FLAG_NONE,GST_VIDEO_FORMAT_RGBA,width,height);retained.push_back(out.resource);
    }
    list->EndQuery(query.Get(),D3D12_QUERY_TYPE_TIMESTAMP,1);list->ResolveQueryData(query.Get(),D3D12_QUERY_TYPE_TIMESTAMP,0,2,timing.Get(),0);check(list->Close(),"Finish interpolation commands");ID3D12CommandList *lists[]={list.Get()};queue->ExecuteCommandLists(1,lists);submitted=true;++serial;check(queue->Signal(fence.Get(),serial),"Signal interpolation completion");
    for(int i=0;i<count;i++)gst_d3d12_memory_set_fence(GST_D3D12_MEMORY_CAST(gst_buffer_peek_memory(outputs[i],0)),fence.Get(),serial,FALSE);
  }
};
extern "C" {
void *mochune_fps_new(GstBuffer *buffer,unsigned w,unsigned h,char *error,size_t cap){try{return new Gpu(buffer,w,h);}catch(const std::exception &e){error_text(error,cap,e.what());return nullptr;}}
int mochune_fps_render(void *ptr,GstBuffer *a,GstBuffer *b,const float *fractions,int count,GstBuffer **out,uint64_t *ns,char *error,size_t cap){try{auto g=(Gpu*)ptr;g->render(a,b,fractions,count,out);*ns=g->last_ns;return 1;}catch(const std::exception &e){error_text(error,cap,e.what());for(int i=0;i<count;i++)if(out[i]){gst_buffer_unref(out[i]);out[i]=nullptr;}auto gpu=(Gpu*)ptr;return gpu->submitted&&!gpu->finish(0)?-1:0;}}
void mochune_fps_free(void *ptr){auto g=(Gpu*)ptr;if(g&&g->finish(2000))delete g;/* A hung device is reclaimed on video-worker exit, never free in-flight storage. */}
int mochune_fps_shader_check(char *error,size_t cap){try{for(auto body:bodies)compile_shader(body);return 1;}catch(const std::exception&e){error_text(error,cap,e.what());return 0;}}
}
