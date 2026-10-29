// Joint-color vector medoid: reject isolated aperture/outlier vectors without
// averaging a foreground edge into the background. Only low-resolution work.
uniform sampler2D previous_frame;
uniform sampler2D next_frame;
uniform sampler2D motion_field;
uniform sampler2D camera_field;
uniform vec2 image_size;
in vec2 uv;
out vec4 result;
vec3 color_at(vec2 at,bool backward){return backward?texture(next_frame,at).rgb:texture(previous_frame,at).rgb;}
float error_at(vec2 vector,bool backward){
    float e=0.0;
    for(int k=0;k<3;++k){
        vec2 at=uv+vec2(float(k-1)*2.0,0)/image_size;
        vec3 a=color_at(at,backward),b=color_at(at+vector,!backward),d=a-b;
        e+=dot(d,d);
    }
    return e/3.0;
}
vec2 refine_direction(bool backward){
    vec2 size=vec2(textureSize(motion_field,0));
    vec4 original=texture(motion_field,uv);
    vec2 center=backward?original.zw:original.xy;
    vec4 camera=texture(camera_field,vec2(0.5));
    vec2 proposal=backward?camera.zw:camera.xy;
    bool camera_valid=dot(proposal*image_size,proposal*image_size)>1.0;
    if(dot(center*image_size,center*image_size)<0.25 && !camera_valid)return center;
    vec3 guide=color_at(uv,backward);
    vec2 candidates[9];float weights[9];float variation=0.0;
    for(int k=0;k<9;++k){
        vec2 offset=k==0?vec2(0):k==1?vec2(-1,0):k==2?vec2(1,0):k==3?vec2(0,-1):k==4?vec2(0,1):k==5?vec2(-2,0):k==6?vec2(2,0):k==7?vec2(0,-2):vec2(0,2);
        vec2 at=clamp(uv+offset/size,0.5/size,vec2(1)-0.5/size);
        vec4 v=texture(motion_field,at);candidates[k]=backward?v.zw:v.xy;
        vec3 delta=guide-color_at(at,backward);
        weights[k]=dot(candidates[k]*image_size,candidates[k]*image_size)<0.25?0.0:1.0/(1.0+128.0*dot(delta,delta));
        if(weights[k]>0.5){vec2 d=(candidates[k]-center)*image_size;variation=max(variation,dot(d,d));}
    }
    if(variation<0.5){
        if(camera_valid && error_at(proposal,backward)+0.000005<error_at(center,backward))return proposal;
        return center;
    }
    float best=1e20;vec2 selected=center;
    for(int k=0;k<9;++k){
        if(weights[k]<0.5)continue;
        float score=0.0;
        for(int j=0;j<9;++j)score+=weights[j]*length((candidates[k]-candidates[j])*image_size);
        score+=0.05*length((candidates[k]-center)*image_size);
        if(score<best){best=score;selected=candidates[k];}
    }
    // A scene-wide consensus fixes ambiguous fragments of a sliding edge;
    // require local photometric agreement before applying it to a subject.
    if(camera_valid && error_at(proposal,backward)+0.000005<error_at(selected,backward))selected=proposal;
    // Recover the local edge phase after regularization, not a distant vector.
    vec2 origin=selected;float match=error_at(selected,backward);
    for(int y=-1;y<=1;++y)for(int x=-1;x<=1;++x){
        vec2 candidate=origin+vec2(float(x),float(y))/image_size;
        float score=error_at(candidate,backward)+float(x*x+y*y)*0.0001;
        if(score<match){match=score;selected=candidate;}
    }
    return match<=error_at(center,backward)+0.05?selected:center;
}
void main(){result=vec4(refine_direction(false),refine_direction(true));}
