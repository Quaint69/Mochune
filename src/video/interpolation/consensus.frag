// Robust camera-motion proposal from 25 fixed grid samples. No readback.
uniform sampler2D motion_field;
uniform vec2 image_size;
in vec2 uv;
out vec4 result;
vec2 dominant(bool backward){
    vec2 vectors[25];int valid=0;
    for(int j=0;j<25;++j){
        vec2 at=(vec2(float(j%5),float(j/5))+0.5)/5.0;
        vec4 m=texture(motion_field,at);
        vectors[j]=backward?m.zw:m.xy;
        if(dot(vectors[j]*image_size,vectors[j]*image_size)>1.0)valid++;
    }
    int best=0;vec2 chosen=vec2(0);
    for(int j=0;j<25;++j){
        if(dot(vectors[j]*image_size,vectors[j]*image_size)<=1.0)continue;
        int votes=0;
        for(int k=0;k<25;++k){vec2 delta=(vectors[j]-vectors[k])*image_size;if(dot(delta,delta)<4.0)votes++;}
        if(votes>best){best=votes;chosen=vectors[j];}
    }
    return best>=8 && float(best)>=float(valid)*0.7?chosen:vec2(0);
}
void main(){result=vec4(dominant(false),dominant(true));}
