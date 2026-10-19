// Meetily's persistent Fun-ASR-Nano helper on the llama.cpp / ggml stack.
//
//   wav(16k mono) -> kaldi fbank -> SAN-M encoder + adaptor (ggml) ->
//   low-frame-rate truncation -> [prefix tokens | audio embeds | suffix tokens]
//   -> Qwen3 LLM (llama.cpp) -> transcription.
//
// This is the whisper.cpp-style native path: no Python at runtime.
// Meetily supplies 16 kHz mono float PCM over a versioned stdin/stdout protocol.

#include "ggml.h"
#include "ggml-cpu.h"
#include "ggml-alloc.h"
#include "ggml-backend.h"
#include "gguf.h"
#include "llama.h"

#include <cmath>
#include <cstdint>
#include <cstdio>
#include <cstring>
#include <map>
#include <memory>
#include <stdexcept>
#include <string>
#include <vector>

#include <utility>

#ifdef _WIN32
#include <fcntl.h>
#include <io.h>
#endif

// ======================= kaldi fbank + LFR =======================
static const int FS=16000, WINLEN=400, SHIFT=160, NFFT=512, NMEL=80, LFR_M=7, LFR_N=6;
static const float PREEMPH=0.97f, LOWF=20.0f, HIGHF=8000.0f;
static inline float mel(float f){ return 1127.0f*logf(1.0f+f/700.0f); }
static void fft(std::vector<float>&re,std::vector<float>&im,int n){
    for(int i=1,j=0;i<n;i++){int b=n>>1;for(;j&b;b>>=1)j^=b;j^=b;if(i<j){std::swap(re[i],re[j]);std::swap(im[i],im[j]);}}
    for(int len=2;len<=n;len<<=1){double a=-2.0*M_PI/len;float wr=cosf(a),wi=sinf(a);
        for(int i=0;i<n;i+=len){float cr=1,ci=0;for(int k=0;k<len/2;k++){
            float ur=re[i+k],ui=im[i+k];float vr=re[i+k+len/2]*cr-im[i+k+len/2]*ci,vi=re[i+k+len/2]*ci+im[i+k+len/2]*cr;
            re[i+k]=ur+vr;im[i+k]=ui+vi;re[i+k+len/2]=ur-vr;im[i+k+len/2]=ui-vi;
            float n2=cr*wr-ci*wi;ci=cr*wi+ci*wr;cr=n2;}}}
}
// returns [T x 560] row-major, sets T
static std::vector<float> compute_fbank(std::vector<float> wav, int & T_out) {
    for (auto & v : wav) v *= 32768.0f;
    std::vector<float> win(WINLEN);
    for (int i=0;i<WINLEN;i++) win[i]=0.54f-0.46f*cosf(2.0f*M_PI*i/(WINLEN-1));
    const int NBIN=NFFT/2+1; float bw=(float)FS/NFFT, ml=mel(LOWF), mh=mel(HIGHF), dm=(mh-ml)/(NMEL+1);
    std::vector<std::vector<float>> fb(NMEL, std::vector<float>(NBIN,0.0f));
    for(int m=0;m<NMEL;m++){float L=ml+m*dm,C=ml+(m+1)*dm,R=ml+(m+2)*dm;
        for(int k=0;k<NBIN;k++){float mf=mel(bw*k); if(mf>L&&mf<R) fb[m][k]=mf<=C?(mf-L)/(C-L):(R-mf)/(R-C);}}
    int N=wav.size(); int T=(N-WINLEN)/SHIFT+1;
    std::vector<std::vector<float>> feat(T, std::vector<float>(NMEL));
    std::vector<float> re(NFFT),im(NFFT),fr(WINLEN);
    const float fl=1.1920929e-07f;
    for(int t=0;t<T;t++){const float*s=wav.data()+t*SHIFT;
        double mn=0;for(int i=0;i<WINLEN;i++)mn+=s[i];mn/=WINLEN;
        for(int i=0;i<WINLEN;i++)fr[i]=s[i]-(float)mn;
        for(int i=WINLEN-1;i>0;i--)fr[i]-=PREEMPH*fr[i-1];fr[0]-=PREEMPH*fr[0];
        for(int i=0;i<NFFT;i++){re[i]=i<WINLEN?fr[i]*win[i]:0.0f;im[i]=0.0f;}
        fft(re,im,NFFT);
        for(int m=0;m<NMEL;m++){float e=0;for(int k=0;k<NBIN;k++)if(fb[m][k]>0)e+=fb[m][k]*(re[k]*re[k]+im[k]*im[k]);
            feat[t][m]=logf(e>fl?e:fl);}}
    // LFR
    const int pad=(LFR_M-1)/2; int T_lfr=(T+LFR_N-1)/LFR_N;
    std::vector<std::vector<float>> pd; pd.reserve(T+pad+LFR_M);
    for(int i=0;i<pad;i++)pd.push_back(feat[0]);
    for(int t=0;t<T;t++)pd.push_back(feat[t]);
    while((int)pd.size()<(T_lfr-1)*LFR_N+LFR_M)pd.push_back(feat[T-1]);
    int D=LFR_M*NMEL; std::vector<float> out((size_t)T_lfr*D);
    for(int i=0;i<T_lfr;i++)for(int j=0;j<LFR_M;j++)
        memcpy(&out[(size_t)i*D+j*NMEL],pd[i*LFR_N+j].data(),NMEL*sizeof(float));
    T_out=T_lfr; return out;
}

// ======================= ggml SAN-M encoder + adaptor =======================
struct cfg { int d_model=512,n_head=4,num_blocks=50,tp_blocks=20,kernel=11,adp_llm=1024,adp_layers=2,adp_head=8; };
struct enc_model { cfg c; ggml_context*ctx_w=nullptr; std::map<std::string,ggml_tensor*> t;
    ggml_tensor* g(const std::string&n){auto it=t.find(n);if(it==t.end()){fprintf(stderr,"missing %s\n",n.c_str());exit(1);}return it->second;} };
static const float LN_EPS=1e-5f;
static bool load_enc(const char*p, enc_model&m){
    gguf_init_params gp={false,&m.ctx_w}; gguf_context*g=gguf_init_from_file(p,gp); if(!g)return false;
    auto rd=[&](const char*k,int d){int i=gguf_find_key(g,k);return i<0?d:(int)gguf_get_val_u32(g,i);};
    m.c.d_model=rd("funasr.enc.output_size",512); m.c.n_head=rd("funasr.enc.attention_heads",4);
    m.c.num_blocks=rd("funasr.enc.num_blocks",50); m.c.tp_blocks=rd("funasr.enc.tp_blocks",20);
    m.c.kernel=rd("funasr.enc.kernel_size",11); m.c.adp_llm=rd("funasr.adp.llm_dim",1024);
    m.c.adp_layers=rd("funasr.adp.n_layer",2); m.c.adp_head=rd("funasr.adp.attention_heads",8);
    int n=gguf_get_n_tensors(g); for(int i=0;i<n;i++){const char*nm=gguf_get_tensor_name(g,i);m.t[nm]=ggml_get_tensor(m.ctx_w,nm);}
    gguf_free(g); return true;
}
static ggml_tensor* lin(ggml_context*c,ggml_tensor*w,ggml_tensor*b,ggml_tensor*x){auto y=ggml_mul_mat(c,w,x);return b?ggml_add(c,y,b):y;}
static ggml_tensor* lnorm(ggml_context*c,ggml_tensor*x,ggml_tensor*g,ggml_tensor*b){return ggml_add(c,ggml_mul(c,ggml_norm(c,x,LN_EPS),g),b);}
static ggml_tensor* sanm_attn(ggml_context*c,enc_model&m,const std::string&p,ggml_tensor*x,int T){
    const int D=m.c.d_model,H=m.c.n_head,dk=D/H,K=m.c.kernel;
    ggml_tensor*qkv=lin(c,m.g(p+"linear_q_k_v.weight"),m.g(p+"linear_q_k_v.bias"),x); size_t nb1=qkv->nb[1];
    ggml_tensor*q=ggml_cont(c,ggml_view_2d(c,qkv,D,T,nb1,0));
    ggml_tensor*k=ggml_cont(c,ggml_view_2d(c,qkv,D,T,nb1,(size_t)D*sizeof(float)));
    ggml_tensor*v=ggml_cont(c,ggml_view_2d(c,qkv,D,T,nb1,(size_t)2*D*sizeof(float)));
    const int pad=(K-1)/2; ggml_tensor*fk=m.g(p+"fsmn_block.weight");
    ggml_tensor*vp=ggml_pad_ext(c,v,0,0,pad,pad,0,0,0,0); ggml_tensor*fsmn=v;
    for(int j=0;j<K;j++){auto sl=ggml_view_2d(c,vp,D,T,vp->nb[1],(size_t)j*vp->nb[1]);
        auto wj=ggml_view_1d(c,fk,D,(size_t)j*fk->nb[1]); fsmn=ggml_add(c,fsmn,ggml_mul(c,ggml_cont(c,sl),wj));}
    q=ggml_permute(c,ggml_reshape_3d(c,q,dk,H,T),0,2,1,3); k=ggml_permute(c,ggml_reshape_3d(c,k,dk,H,T),0,2,1,3);
    ggml_tensor*vh=ggml_cont(c,ggml_permute(c,ggml_reshape_3d(c,v,dk,H,T),1,2,0,3));
    ggml_tensor*kq=ggml_soft_max(c,ggml_scale(c,ggml_mul_mat(c,k,q),1.0f/sqrtf((float)dk)));
    ggml_tensor*o=ggml_cont_2d(c,ggml_permute(c,ggml_mul_mat(c,vh,kq),0,2,1,3),D,T);
    return ggml_add(c,lin(c,m.g(p+"linear_out.weight"),m.g(p+"linear_out.bias"),o),fsmn);
}
static ggml_tensor* sanm_layer(ggml_context*c,enc_model&m,const std::string&p,ggml_tensor*x,int T,bool res){
    auto r=x; auto h=lnorm(c,x,m.g(p+"norm1.weight"),m.g(p+"norm1.bias"));
    auto sa=sanm_attn(c,m,p+"self_attn.",h,T); x=res?ggml_add(c,r,sa):sa; r=x;
    h=lnorm(c,x,m.g(p+"norm2.weight"),m.g(p+"norm2.bias"));
    h=lin(c,m.g(p+"feed_forward.w_1.weight"),m.g(p+"feed_forward.w_1.bias"),h); h=ggml_relu(c,h);
    h=lin(c,m.g(p+"feed_forward.w_2.weight"),m.g(p+"feed_forward.w_2.bias"),h); return ggml_add(c,r,h);
}
static ggml_tensor* adp_layer(ggml_context*c,enc_model&m,const std::string&p,ggml_tensor*x,int T){
    const int D=m.c.adp_llm,H=m.c.adp_head,dk=D/H; auto r=x;
    auto h=lnorm(c,x,m.g(p+"norm1.weight"),m.g(p+"norm1.bias"));
    auto q=ggml_permute(c,ggml_reshape_3d(c,lin(c,m.g(p+"self_attn.linear_q.weight"),m.g(p+"self_attn.linear_q.bias"),h),dk,H,T),0,2,1,3);
    auto k=ggml_permute(c,ggml_reshape_3d(c,lin(c,m.g(p+"self_attn.linear_k.weight"),m.g(p+"self_attn.linear_k.bias"),h),dk,H,T),0,2,1,3);
    auto vh=ggml_cont(c,ggml_permute(c,ggml_reshape_3d(c,lin(c,m.g(p+"self_attn.linear_v.weight"),m.g(p+"self_attn.linear_v.bias"),h),dk,H,T),1,2,0,3));
    auto kq=ggml_soft_max(c,ggml_scale(c,ggml_mul_mat(c,k,q),1.0f/sqrtf((float)dk)));
    auto o=ggml_cont_2d(c,ggml_permute(c,ggml_mul_mat(c,vh,kq),0,2,1,3),D,T);
    x=ggml_add(c,r,lin(c,m.g(p+"self_attn.linear_out.weight"),m.g(p+"self_attn.linear_out.bias"),o)); r=x;
    h=lnorm(c,x,m.g(p+"norm2.weight"),m.g(p+"norm2.bias"));
    h=lin(c,m.g(p+"feed_forward.w_1.weight"),m.g(p+"feed_forward.w_1.bias"),h); h=ggml_relu(c,h);
    h=lin(c,m.g(p+"feed_forward.w_2.weight"),m.g(p+"feed_forward.w_2.bias"),h); return ggml_add(c,r,h);
}
static void add_posenc(std::vector<float>&x,int T,int depth){
    double inc=log(10000.0)/(depth/2.0-1.0);
    for(int t=0;t<T;t++){double pos=t+1;for(int i=0;i<depth/2;i++){double its=exp(i*-inc),st=pos*its;
        x[(size_t)t*depth+i]+=(float)sin(st);x[(size_t)t*depth+depth/2+i]+=(float)cos(st);}}
}
// fbank [T x F] -> adaptor out [T x adp_llm] row-major
static std::vector<float> run_encoder(enc_model&m,std::vector<float> fbank,int T,int F,int&Dout){
    float sc=sqrtf((float)m.c.d_model); for(auto&v:fbank)v*=sc; add_posenc(fbank,T,F);
    ggml_backend_t be=ggml_backend_cpu_init();
    ggml_init_params cp={(size_t)1024*1024*1024,nullptr,true}; ggml_context*c=ggml_init(cp);
    ggml_tensor*inp=ggml_new_tensor_2d(c,GGML_TYPE_F32,F,T); ggml_set_input(inp);
    ggml_tensor*x=sanm_layer(c,m,"audio_encoder.encoders0.0.",inp,T,false);
    for(int i=0;i<m.c.num_blocks-1;i++) x=sanm_layer(c,m,"audio_encoder.encoders."+std::to_string(i)+".",x,T,true);
    x=lnorm(c,x,m.g("audio_encoder.after_norm.weight"),m.g("audio_encoder.after_norm.bias"));
    for(int i=0;i<m.c.tp_blocks;i++) x=sanm_layer(c,m,"audio_encoder.tp_encoders."+std::to_string(i)+".",x,T,true);
    x=lnorm(c,x,m.g("audio_encoder.tp_norm.weight"),m.g("audio_encoder.tp_norm.bias"));
    x=lin(c,m.g("audio_adaptor.linear1.weight"),m.g("audio_adaptor.linear1.bias"),x); x=ggml_relu(c,x);
    x=lin(c,m.g("audio_adaptor.linear2.weight"),m.g("audio_adaptor.linear2.bias"),x);
    for(int i=0;i<m.c.adp_layers;i++) x=adp_layer(c,m,"audio_adaptor.blocks."+std::to_string(i)+".",x,T);
    ggml_set_output(x);
    ggml_cgraph*gf=ggml_new_graph_custom(c,32768,false); ggml_build_forward_expand(gf,x);
    ggml_gallocr_t ga=ggml_gallocr_new(ggml_backend_cpu_buffer_type()); ggml_gallocr_alloc_graph(ga,gf);
    ggml_backend_tensor_set(inp,fbank.data(),0,ggml_nbytes(inp));
    ggml_backend_cpu_set_n_threads(be,8); ggml_backend_graph_compute(be,gf);
    Dout=(int)x->ne[0]; std::vector<float> out((size_t)Dout*T); ggml_backend_tensor_get(x,out.data(),0,ggml_nbytes(x));
    ggml_gallocr_free(ga); ggml_free(c); ggml_backend_free(be); return out;
}

// ======================= LLM (llama.cpp) =======================
static int decode_batch(llama_context*ctx,int n,llama_token*tok,float*embd,int n_embd,int&n_past,bool last_logits){
    std::vector<llama_pos> pos(n); std::vector<int32_t> nsid(n,1);
    std::vector<llama_seq_id> s0(1,0); std::vector<llama_seq_id*> sid(n); std::vector<int8_t> lg(n,0);
    for(int i=0;i<n;i++){pos[i]=n_past+i;sid[i]=s0.data();}
    if(last_logits) lg[n-1]=1;
    llama_batch b={n,tok,embd,pos.data(),nsid.data(),sid.data(),lg.data()};
    int r=llama_decode(ctx,b); n_past+=n; return r;
}

static constexpr uint16_t PROTOCOL_VERSION = 1;
static constexpr uint16_t OP_TRANSCRIBE = 1;
static constexpr uint16_t OP_PING = 2;
static constexpr uint16_t OP_SHUTDOWN = 3;
static constexpr uint32_t MAX_SAMPLES = 30 * 16000;

static bool read_exact(void * data, size_t size) {
    return size == 0 || fread(data, 1, size, stdin) == size;
}

static bool write_exact(const void * data, size_t size) {
    return size == 0 || fwrite(data, 1, size, stdout) == size;
}

template<typename T>
static bool read_le(T & value) {
    unsigned char bytes[sizeof(T)];
    if (!read_exact(bytes, sizeof(T))) return false;
    value = 0;
    for (size_t i = 0; i < sizeof(T); ++i) {
        value |= static_cast<T>(bytes[i]) << (8 * i);
    }
    return true;
}

template<typename T>
static bool write_le(T value) {
    unsigned char bytes[sizeof(T)];
    for (size_t i = 0; i < sizeof(T); ++i) {
        bytes[i] = static_cast<unsigned char>((value >> (8 * i)) & 0xff);
    }
    return write_exact(bytes, sizeof(T));
}

static bool write_response(uint64_t request_id, uint16_t status,
                           const std::string & text, const std::string & error) {
    if (!write_exact("MFAR", 4) ||
        !write_le<uint16_t>(PROTOCOL_VERSION) ||
        !write_le<uint16_t>(status) ||
        !write_le<uint64_t>(request_id) ||
        !write_le<uint32_t>(static_cast<uint32_t>(text.size())) ||
        !write_le<uint32_t>(static_cast<uint32_t>(error.size())) ||
        !write_exact(text.data(), text.size()) ||
        !write_exact(error.data(), error.size())) {
        return false;
    }
    return fflush(stdout) == 0;
}

struct runtime {
    enc_model encoder;
    llama_model * model = nullptr;
    llama_context * context = nullptr;
    llama_sampler * sampler = nullptr;
    const llama_vocab * vocab = nullptr;

    ~runtime() {
        if (sampler) llama_sampler_free(sampler);
        if (context) llama_free(context);
        if (model) llama_model_free(model);
        if (encoder.ctx_w) ggml_free(encoder.ctx_w);
    }

    bool load(const std::string & encoder_path, const std::string & decoder_path) {
        ggml_time_init();
        if (!load_enc(encoder_path.c_str(), encoder)) {
            fprintf(stderr, "failed to load FUN-ASR encoder\n");
            return false;
        }
        ggml_backend_load_all();
        llama_model_params model_params = llama_model_default_params();
        model_params.n_gpu_layers = 0;
        model = llama_model_load_from_file(decoder_path.c_str(), model_params);
        if (!model) {
            fprintf(stderr, "failed to load FUN-ASR decoder\n");
            return false;
        }
        vocab = llama_model_get_vocab(model);
        llama_context_params context_params = llama_context_default_params();
        context_params.n_ctx = 2048;
        context_params.n_batch = 2048;
        context_params.n_ubatch = 2048;
        context = llama_init_from_model(model, context_params);
        if (!context) {
            fprintf(stderr, "failed to create FUN-ASR llama context\n");
            return false;
        }
        sampler = llama_sampler_chain_init(llama_sampler_chain_default_params());
        llama_sampler_chain_add(sampler, llama_sampler_init_greedy());
        return true;
    }

    std::vector<llama_token> tokenize(const std::string & text) const {
        int count = -llama_tokenize(
            vocab, text.data(), static_cast<int32_t>(text.size()),
            nullptr, 0, false, true);
        std::vector<llama_token> tokens(static_cast<size_t>(count));
        llama_tokenize(
            vocab, text.data(), static_cast<int32_t>(text.size()),
            tokens.data(), count, false, true);
        return tokens;
    }

    std::string transcribe(const std::vector<float> & audio, const std::string & language) {
        if (audio.size() < WINLEN) {
            return {};
        }
        // The upstream Nano runtime uses the Chinese instruction as its
        // language-neutral/default transcription prompt.
        const char * instruction = "语音转写：";
        if (language == "en") {
            instruction = "Speech transcription: ";
        } else if (language == "ja") {
            instruction = "音声を書き起こしてください：";
        }
        const std::string prefix =
            "<|im_start|>system\nYou are a helpful assistant.<|im_end|>\n"
            "<|im_start|>user\n" + std::string(instruction);
        const std::string suffix = "<|im_end|>\n<|im_start|>assistant\n";
        auto pre = tokenize(prefix);
        auto suf = tokenize(suffix);

        int frames = 0;
        auto fbank = compute_fbank(audio, frames);
        int embedding_size = 0;
        auto embeddings = run_encoder(encoder, fbank, frames, 560, embedding_size);
        int output_length = 1 + (frames - 3 + 2) / 2;
        output_length = 1 + (output_length - 3 + 2) / 2;
        int audio_tokens = (output_length - 1) / 2 + 1;

        llama_memory_clear(llama_get_memory(context), true);
        llama_sampler_reset(sampler);
        int past = 0;
        if (decode_batch(context, static_cast<int>(pre.size()), pre.data(),
                         nullptr, 0, past, false) != 0 ||
            decode_batch(context, audio_tokens, nullptr, embeddings.data(),
                         embedding_size, past, false) != 0 ||
            decode_batch(context, static_cast<int>(suf.size()), suf.data(),
                         nullptr, 0, past, true) != 0) {
            throw std::runtime_error("FUN-ASR prompt decoding failed");
        }

        std::string text;
        llama_token token = llama_sampler_sample(sampler, context, -1);
        for (int i = 0; i < 512; ++i) {
            if (llama_vocab_is_eog(vocab, token)) break;
            char buffer[256];
            int length = llama_token_to_piece(
                vocab, token, buffer, sizeof(buffer), 0, true);
            if (length > 0) text.append(buffer, static_cast<size_t>(length));
            if (decode_batch(context, 1, &token, nullptr, 0, past, true) != 0) {
                throw std::runtime_error("FUN-ASR token decoding failed");
            }
            token = llama_sampler_sample(sampler, context, -1);
        }
        return text;
    }
};

int main(int argc, char ** argv) {
#ifdef _WIN32
    _setmode(_fileno(stdin), _O_BINARY);
    _setmode(_fileno(stdout), _O_BINARY);
#endif
    std::string encoder_path;
    std::string decoder_path;
    for (int i = 1; i < argc; ++i) {
        if (!strcmp(argv[i], "--encoder") && i + 1 < argc) {
            encoder_path = argv[++i];
        } else if (!strcmp(argv[i], "--decoder") && i + 1 < argc) {
            decoder_path = argv[++i];
        } else {
            fprintf(stderr, "usage: %s --encoder encoder.gguf --decoder decoder.gguf\n", argv[0]);
            return 2;
        }
    }
    if (encoder_path.empty() || decoder_path.empty()) {
        fprintf(stderr, "missing --encoder or --decoder\n");
        return 2;
    }

    runtime engine;
    if (!engine.load(encoder_path, decoder_path)) return 1;
    fprintf(stderr, "FUN-ASR helper ready\n");

    for (;;) {
        char magic[4];
        if (!read_exact(magic, sizeof(magic))) break;
        uint16_t version = 0;
        uint16_t opcode = 0;
        uint64_t request_id = 0;
        uint32_t sample_rate = 0;
        uint16_t language_length = 0;
        uint32_t sample_count = 0;
        if (memcmp(magic, "MFAS", 4) != 0 ||
            !read_le<uint16_t>(version) ||
            !read_le<uint16_t>(opcode) ||
            !read_le<uint64_t>(request_id) ||
            !read_le<uint32_t>(sample_rate) ||
            !read_le<uint16_t>(language_length) ||
            !read_le<uint32_t>(sample_count)) {
            break;
        }
        if (version != PROTOCOL_VERSION) {
            write_response(request_id, 1, {}, "unsupported protocol version");
            break;
        }
        if (language_length > 64 || sample_count > MAX_SAMPLES) {
            write_response(request_id, 1, {}, "request exceeds size limits");
            break;
        }
        std::string language(language_length, '\0');
        std::vector<float> audio(sample_count);
        if (!read_exact(language.data(), language.size()) ||
            !read_exact(audio.data(), audio.size() * sizeof(float))) {
            break;
        }

        if (opcode == OP_PING) {
            if (!write_response(request_id, 0, "pong", {})) break;
        } else if (opcode == OP_SHUTDOWN) {
            write_response(request_id, 0, {}, {});
            break;
        } else if (opcode == OP_TRANSCRIBE) {
            if (sample_rate != 16000) {
                if (!write_response(request_id, 1, {}, "sample rate must be 16000 Hz")) break;
                continue;
            }
            try {
                const int64_t started = ggml_time_us();
                std::string text = engine.transcribe(audio, language);
                fprintf(stderr, "[request %llu] %.2fs\n",
                        static_cast<unsigned long long>(request_id),
                        (ggml_time_us() - started) / 1e6);
                if (!write_response(request_id, 0, text, {})) break;
            } catch (const std::exception & error) {
                if (!write_response(request_id, 1, {}, error.what())) break;
            }
        } else {
            if (!write_response(request_id, 1, {}, "unknown operation")) break;
        }
    }
    return 0;
}
