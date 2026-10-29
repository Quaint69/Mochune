// Bounded 1/8-resolution box-filtered search images; GPU only.
uniform sampler2D previous_frame;
uniform sampler2D next_frame;
uniform vec2 image_size;
uniform float fraction;
in vec2 uv;
out vec4 result;
void main(){
    vec3 sum=vec3(0);
    vec2 scale=image_size/ceil(image_size/8.0);
    for(int y=0;y<4;++y)for(int x=0;x<4;++x){
        vec2 p=uv+(vec2(float(x),float(y))/4.0-0.375)*scale/image_size;
        sum+=fraction<0.5?texture(previous_frame,p).rgb:texture(next_frame,p).rgb;
    }
    result=vec4(sum/16.0,1);
}
