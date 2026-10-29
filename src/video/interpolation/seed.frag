// Fill only the inverse-warp starting guesses, never overwrite raw motion.
// This tiny pass lets thin shapes travel into previously empty output pixels.
uniform sampler2D previous_frame;
uniform sampler2D next_frame;
uniform sampler2D motion_field;
uniform vec2 image_size;
in vec2 uv;
out vec4 result;
float quality(vec2 at,vec2 motion,bool backward){
    if(dot(motion*image_size,motion*image_size)<0.25)return 0.0;
    vec2 end=at+motion;
    if(any(lessThan(end,vec2(0)))||any(greaterThan(end,vec2(1))))return 0.0;
    vec4 other=texture(motion_field,end);
    vec2 reverse=backward?other.xy:other.zw;
    float error=0.0,mean=0.0,squared=0.0;
    for(int k=0;k<5;++k){
        vec2 offset=k==0?vec2(0):k==1?vec2(-2,0):k==2?vec2(2,0):k==3?vec2(0,-2):vec2(0,2);
        vec2 p=at+offset/image_size,q=end+offset/image_size;
        vec3 a=backward?texture(next_frame,p).rgb:texture(previous_frame,p).rgb;
        vec3 b=backward?texture(previous_frame,q).rgb:texture(next_frame,q).rgb;
        vec3 delta=a-b;error+=dot(delta,delta);
        float l=dot(a,vec3(0.2126,0.7152,0.0722));mean+=l;squared+=l*l;
    }
    float contrast=max(0.0,squared/5.0-(mean/5.0)*(mean/5.0));
    // Uniform patches contain no displacement evidence; do not let them
    // override a nearby edge-supported vector just because their colors match.
    float feature=0.03+0.97*smoothstep(0.0001,0.015,contrast);
    float photo=1.0/(1.0+64.0*error/5.0);
    float cycle=length((motion+reverse)*image_size);
    float consistency=dot(reverse*image_size,reverse*image_size)<0.25?0.08:1.0/(1.0+cycle*cycle*0.25);
    return photo*consistency*feature;
}
void main(){
    vec4 center=texture(motion_field,uv),seed=center;
    vec2 cell=1.0/vec2(textureSize(motion_field,0));
    if(dot(center.xy*image_size,center.xy*image_size)>0.25&&length((center.xy+center.zw)*image_size)<0.5){
        float variation=0.0;
        for(int d=0;d<4;++d){vec2 axis=d==0?vec2(-1,0):d==1?vec2(1,0):d==2?vec2(0,-1):vec2(0,1);vec4 delta=(texture(motion_field,uv+axis*cell)-center)*image_size.xyxy;variation=max(variation,dot(delta,delta));}
        if(variation<1.0){result=center;return;}
    }
    float sf=quality(uv,center.xy,false),sb=quality(uv,center.zw,true);
    if(min(sf,sb)>0.95){result=center;return;}
    vec2 texel=1.0/vec2(textureSize(motion_field,0));
    for(int r=1;r<=4;++r)for(int d=0;d<4;++d){
        vec2 axis=d==0?vec2(-1,0):d==1?vec2(1,0):d==2?vec2(0,-1):vec2(0,1);
        vec2 at=clamp(uv+axis*texel*float(r),texel*0.5,vec2(1)-texel*0.5);
        vec4 v=texture(motion_field,at);
        float f=quality(at,v.xy,false)/(1.0+float(r)*0.35);
        float b=quality(at,v.zw,true)/(1.0+float(r)*0.35);
        if(f>sf){sf=f;seed.xy=v.xy;}
        if(b>sb){sb=b;seed.zw=v.zw;}
    }
    result=seed;
}
