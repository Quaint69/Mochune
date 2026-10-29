uniform sampler2D previous_frame;
uniform sampler2D next_frame;
uniform sampler2D motion_field;
uniform sampler2D seed_field;
uniform vec2 image_size;
uniform float fraction;
in vec2 uv;
out vec4 result;
float difference(vec3 a,vec3 b){vec3 d=a-b;return dot(d,d);}
bool inside(vec2 p){return all(greaterThanEqual(p,vec2(0)))&&all(lessThanEqual(p,vec2(1)));}
// Joint color/spatial upsampling: foreground vectors must not be averaged
// indiscriminately with stationary background vectors across a silhouette.
vec2 edge_flow(vec2 at,bool backward,vec2 fallback){
    at=clamp(at,vec2(0),vec2(1));
    vec2 size=vec2(textureSize(motion_field,0)),p=at*size-0.5,f=fract(p);
    ivec2 base=ivec2(floor(p)),limit=textureSize(motion_field,0)-ivec2(1);
    vec3 guide=backward?texture(next_frame,at).rgb:texture(previous_frame,at).rgb;
    vec3 other=backward?texture(previous_frame,at).rgb:texture(next_frame,at).rgb;
    bool moving=difference(guide,other)>0.003;
    vec2 sum=vec2(0);float weights=0.0;
    for(int y=0;y<=1;++y)for(int x=0;x<=1;++x){
        ivec2 cell=clamp(base+ivec2(x,y),ivec2(0),limit);
        vec2 coord=(vec2(cell)+0.5)/size;
        vec4 field=texelFetch(motion_field,cell,0);
        vec2 v=backward?field.zw:field.xy;
        if(moving&&dot(v*image_size,v*image_size)<0.25)continue;
        vec3 color=backward?texture(next_frame,coord).rgb:texture(previous_frame,coord).rgb;
        float spatial=(x==0?1.0-f.x:f.x)*(y==0?1.0-f.y:f.y);
        float weight=spatial/(1.0+48.0*difference(color,guide));
        sum+=v*weight;weights+=weight;
    }
    return weights>0.0001?sum/weights:fallback;
}
float confidence(vec2 at,vec2 motion,bool backward,vec3 color){
    vec2 end=at+motion;
    if(!inside(at)||!inside(end))return 0.0;
    vec3 matched=backward?texture(previous_frame,end).rgb:texture(next_frame,end).rgb;
    float photo=1.0-smoothstep(0.003,0.08,difference(color,matched));
    vec4 reverse=texture(seed_field,end);
    vec2 back=backward?reverse.xy:reverse.zw;
    float distance=length(motion*image_size);
    float cycle=length((motion+back)*image_size);
    float consistency=1.0-smoothstep(1.5+distance*0.1,3.0+distance*0.3,cycle);
    // A featureless reverse block is unknown, not proof that the motion is bad.
    if(dot(back*image_size,back*image_size)<0.25&&distance>2.0)consistency=0.5;
    return photo*consistency;
}
void main(){
    vec3 original_a=texture(previous_frame,uv).rgb,original_b=texture(next_frame,uv).rgb;
    if(fraction<=0.0){result=vec4(original_a,1);return;}
    if(fraction>=1.0){result=vec4(original_b,1);return;}
    vec4 seed=texture(seed_field,uv);
    if(dot(seed.xy*image_size,seed.xy*image_size)+dot(seed.zw*image_size,seed.zw*image_size)<0.25&&difference(original_a,original_b)<0.0001){result=vec4(mix(original_a,original_b,fraction),1);return;}
    vec2 f=seed.xy,b=seed.zw;
    f=edge_flow(uv-f*fraction,false,f);
    b=edge_flow(uv-b*(1.0-fraction),true,b);
    vec2 pa=uv-f*fraction,pb=uv-b*(1.0-fraction);
    vec3 a=texture(previous_frame,clamp(pa,vec2(0),vec2(1))).rgb;
    vec3 z=texture(next_frame,clamp(pb,vec2(0),vec2(1))).rgb;
    float ca=confidence(pa,f,false,a),cb=confidence(pb,b,true,z);
    // Refinement can cross an occlusion edge into a stationary block. Keep
    // the consistency seed hypothesis if it explains the two pictures better.
    vec2 df=(f-seed.xy)*image_size,db=(b-seed.zw)*image_size;
    if(dot(df,df)>0.25){
        vec2 p=uv-seed.xy*fraction;
        vec3 color=texture(previous_frame,clamp(p,vec2(0),vec2(1))).rgb;
        float c=confidence(p,seed.xy,false,color);
        if(c>=ca){pa=p;f=seed.xy;a=color;ca=c;}
    }
    if(dot(db,db)>0.25){
        vec2 p=uv-seed.zw*(1.0-fraction);
        vec3 color=texture(next_frame,clamp(p,vec2(0),vec2(1))).rgb;
        float c=confidence(p,seed.zw,true,color);
        if(c>=cb){pb=p;b=seed.zw;z=color;cb=c;}
    }
    // Independent forward/backward color agreement is useful evidence even
    // where a featureless reverse vector cannot establish a reliable cycle.
    if(difference(a,z)<0.003){result=vec4(mix(a,z,fraction),1);return;}
    if(max(ca,cb)<0.35){result=vec4(fraction<0.5?original_a:original_b,1);return;}
    // At disocclusions choose the supported surface; don't crossfade two edges.
    if(difference(a,z)>0.04){
        if(ca>cb*1.5)result=vec4(a,1);
        else if(cb>ca*1.5)result=vec4(z,1);
        else result=vec4(fraction<0.5?a:z,1);
    }else{
        float wa=(1.0-fraction)*ca,wb=fraction*cb;
        result=vec4((a*wa+z*wb)/max(wa+wb,0.00001),1);
    }
}
