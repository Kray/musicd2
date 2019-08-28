#include "musicd.h"

static enum AVSampleFormat find_sample_fmt(
    enum AVSampleFormat src_fmt,
    const enum AVSampleFormat *dst_fmts
) {
    if (!dst_fmts) {
        return src_fmt;
    }

    for (const enum AVSampleFormat *iter = dst_fmts; *iter != -1; ++iter) {
        if (*iter == src_fmt) {
            return src_fmt;
        }
    }

    // Return first supported sample format
    return *dst_fmts;
}

static int find_sample_rate(int sample_rate, const int *sample_rates) {
    if (!sample_rates) {
        return sample_rate;
    }

    int closest = 0;

    for (const int *iter = sample_rates; *iter != 0; ++iter) {
        if (*iter == sample_rate) {
            return sample_rate;
        }

        if (abs(*iter - sample_rate) < abs(closest - sample_rate)) {
            closest = *iter;
        }
    }

    return closest;
}

static int audio_stream_write_callback(void *opaque, uint8_t *buf, int buf_size) {
    struct AudioStream *self = (struct AudioStream *)opaque;
    return self->write_callback(self->write_opaque, buf, buf_size);
}

struct AudioStream *audio_stream_open(const struct AudioStreamOptions *options) {
    int result;
    char args[512];

    struct AudioStream *self = malloc(sizeof(struct AudioStream));
    memset(self, 0, sizeof(struct AudioStream));

    // TODO track index

    result = avformat_open_input(&self->in_ctx, options->path, NULL, NULL);
    if (result < 0) {
        lav_error("avformat_open_input", result);
        goto fail;
    }
    
    result = avformat_find_stream_info(self->in_ctx, NULL);
    if (result < 0) {
        lav_error("avformat_find_stream_info", result);
        goto fail;
    }

    if (self->in_ctx->nb_streams <= (uint32_t)options->stream_index) {
        lav_error("audio stream doesn't exist", 0);
        goto fail;
    }

    self->in_stream = self->in_ctx->streams[options->stream_index];

    self->decoder = avcodec_find_decoder(self->in_stream->codecpar->codec_id);
    if (!self->decoder) {
        lav_error("avcodec_find_decoder", result);
        goto fail;
    }

    self->dec_ctx = avcodec_alloc_context3(self->decoder);

    if (avcodec_parameters_to_context(self->dec_ctx, self->in_stream->codecpar)) {
        lav_error("avcodec_parameters_to_context", result);
        goto fail;
    }

    if (avcodec_open2(self->dec_ctx, self->decoder, NULL)) {
        lav_error("avcodec_open2", result);
        goto fail;
    }

    if (!self->dec_ctx->channel_layout) {
        self->dec_ctx->channel_layout = av_get_default_channel_layout(self->dec_ctx->channels);
    }

    if (options->start > 0) {
        int64_t seek_pos = options->start / av_q2d(self->in_stream->time_base);
        result = av_seek_frame(self->in_ctx, 0, seek_pos, 0);
        if (result < 0) {
            lav_error("av_seek_frame", result);
            goto fail;
        }
    }

    if (options->length > 0) {
        self->end_pts = (options->start + options->length) / av_q2d(self->in_stream->time_base);
    }

    av_dump_format(self->in_ctx, 0, options->path, 0);

    self->out_ctx = avformat_alloc_context();

    self->out_ctx->oformat = av_guess_format(options->target_codec, NULL, NULL);
    if (!self->out_ctx->oformat) {
        lav_error("av_guess_format", 0);
        goto fail;
    }

    self->out_stream = avformat_new_stream(self->out_ctx, NULL);
    if (!self->out_stream) {
        lav_error("avformat_new_stream", 0);
    }

    // TODO copy metadata

    self->encoder = avcodec_find_encoder(AV_CODEC_ID_MP3);
    if (!self->encoder) {
        lav_error("avcodec_find_encoder", 0);
        goto fail;
    }

    self->enc_ctx = avcodec_alloc_context3(self->encoder);

    self->enc_ctx->sample_fmt = find_sample_fmt(
        self->dec_ctx->sample_fmt,
        self->encoder->sample_fmts);
    self->enc_ctx->sample_rate = find_sample_rate(
        self->dec_ctx->sample_rate,
        self->encoder->supported_samplerates);
    self->enc_ctx->channel_layout = self->dec_ctx->channel_layout;
    self->enc_ctx->channels = av_get_channel_layout_nb_channels(self->enc_ctx->channel_layout);

    result = avcodec_open2(self->enc_ctx, self->encoder, NULL);
    if (result < 0) {
        lav_error("avcodec_open2", result);
        goto fail;
    }

    result = avcodec_parameters_from_context(self->out_stream->codecpar, self->enc_ctx);
    if (result < 0) {
        lav_error("avcodec_parameters_from_context", result);
        goto fail;
    }

    self->out_stream->time_base = self->enc_ctx->time_base;

    uint8_t *out_iobuf = av_mallocz(4096);
    self->out_ioctx = avio_alloc_context(
        out_iobuf, 4096, 1, (void *)self, NULL, audio_stream_write_callback, NULL);
    if (!self->out_ioctx) {
        lav_error("avio_alloc_context", 0);
        goto fail;
    }

    self->out_ctx->pb = self->out_ioctx;
    
    av_dump_format(self->out_ctx, 0, "", 1);

    const AVFilter *abuffer = avfilter_get_by_name("abuffer");
    const AVFilter *aformat = avfilter_get_by_name("aformat");
    const AVFilter *abuffersink = avfilter_get_by_name("abuffersink");

    if (!abuffer) {
        lav_error("av filter abuffer not found", 0);
        goto fail;
    }

    if (!aformat) {
        lav_error("av filter aformat not found", 0);
        goto fail;
    }

    if (!abuffersink) {
        lav_error("av filter abuffersink not found", 0);
        goto fail;
    }

    self->filter_graph = avfilter_graph_alloc();

    snprintf(args, sizeof(args),
        "time_base=%d/%d:sample_rate=%d:sample_fmt=%s:channel_layout=0x%" PRIx64,
        self->dec_ctx->time_base.num, self->dec_ctx->time_base.den, self->dec_ctx->sample_rate,
        av_get_sample_fmt_name(self->dec_ctx->sample_fmt),
        self->dec_ctx->channel_layout);

    result = avfilter_graph_create_filter(
        &self->abuffer_ctx, abuffer, "in", args, NULL, self->filter_graph);
    if (result < 0) {
        lav_error("avfilter_graph_create_filter", result);
        goto fail;
    }

    snprintf(args,
        sizeof(args),
        "sample_fmts=%s:sample_rates=%d:channel_layouts=0x%" PRIx64,
        av_get_sample_fmt_name(self->enc_ctx->sample_fmt),
        self->enc_ctx->sample_rate,
        self->enc_ctx->channel_layout);

    result = avfilter_graph_create_filter(
        &self->aformat_ctx, aformat, NULL, args, NULL, self->filter_graph);
    if (result < 0) {
        lav_error("avfilter_graph_create_filter", result);
        goto fail;
    }

    result = avfilter_graph_create_filter(
        &self->abuffersink_ctx, abuffersink, "out", NULL, NULL, self->filter_graph);
    if (result < 0) {
        lav_error("avfilter_graph_create_filter", result);
        goto fail;
    }

    result = avfilter_link(self->abuffer_ctx, 0, self->aformat_ctx, 0);
    if (result < 0) {
        lav_error("avfilter_link", result);
        goto fail;
    }

    result = avfilter_link(self->aformat_ctx, 0, self->abuffersink_ctx, 0);
    if (result < 0) {
        lav_error("avfilter_link", result);
        goto fail;
    }

    result = avfilter_graph_config(self->filter_graph, NULL);
    if (result < 0) {
        lav_error("avfilter_graph_config", result);
        goto fail;
    }

    av_buffersink_set_frame_size(self->abuffersink_ctx, self->enc_ctx->frame_size);

    return self;

fail:
    audio_stream_close(self);
    return NULL;
}

#define STREAM_ERROR -1
#define STREAM_EOF 0
#define STREAM_AGAIN 1
#define STREAM_OK 2

static int demux_decode(struct AudioStream *self, AVPacket *in_packet) {
    int result = av_read_frame(self->in_ctx, in_packet);

    if (result == AVERROR_EOF) {
        goto eof;
    } else if (result < 0) {
        lav_error("av_read_frame", result);
        return STREAM_ERROR;
    }

    if (in_packet->stream_index != self->in_stream->index) {
        return STREAM_AGAIN;
    }

    av_packet_rescale_ts(in_packet, self->in_stream->time_base, self->dec_ctx->time_base);

    if (self->end_pts > 0 && in_packet->pts > self->end_pts) {
        // Reached track end
        goto eof;
    }

    result = avcodec_send_packet(self->dec_ctx, in_packet);
    if (result < 0) {
        lav_error("avcodec_send_packet", result);
        return STREAM_ERROR;
    }

    return STREAM_OK;

eof:
    result = avcodec_send_packet(self->dec_ctx, NULL);
    if (result < 0) {
        lav_error("avcodec_send_packet", result);
        return STREAM_ERROR;
    }

    return STREAM_EOF;
}

static int decode_resample(struct AudioStream *self, AVFrame *in_frame) {
    int result = avcodec_receive_frame(self->dec_ctx, in_frame);

    if (result == AVERROR(EAGAIN)) {
        return STREAM_AGAIN;
    } else if (result == AVERROR_EOF) {
        goto eof;
    } else if (result < 0) {
        lav_error("avcodec_receive_frame", result);
        return STREAM_ERROR;
    }

    result = av_buffersrc_add_frame_flags(self->abuffer_ctx, in_frame, 0);
    if (result < 0) {
        lav_error("av_buffersrc_add_frame_flags", result);
        return STREAM_ERROR;
    }

    return STREAM_OK;

eof:
    result = av_buffersrc_add_frame_flags(self->abuffer_ctx, NULL, 0);
    if (result < 0) {
        lav_error("av_buffersrc_add_frame_flags", result);
        return STREAM_ERROR;
    }
    
    return STREAM_EOF;
}

static int resample_encode(struct AudioStream *self, AVFrame *out_frame) {
    int result = av_buffersink_get_frame(self->abuffersink_ctx, out_frame);

    if (result == AVERROR(EAGAIN)) {
        return STREAM_AGAIN;
    } else if (result == AVERROR_EOF) {
        goto eof;
    } else if (result < 0) {
        lav_error("av_buffersink_get_frame", result);
        return STREAM_ERROR;
    }

    result = avcodec_send_frame(self->enc_ctx, out_frame);
    if (result < 0) {
        lav_error("avcodec_send_frame", result);
        return STREAM_ERROR;
    }

    return STREAM_OK;

eof:
    result = avcodec_send_frame(self->enc_ctx, NULL);
    if (result < 0) {
        lav_error("avcodec_send_frame", result);
        return STREAM_ERROR;
    }

    return STREAM_EOF;
}

static int encode_mux(struct AudioStream *self, AVPacket *enc_packet) {
    int result = avcodec_receive_packet(self->enc_ctx, enc_packet);

    if (result == AVERROR(EAGAIN)) {
        return STREAM_AGAIN;
    } else if (result == AVERROR_EOF) {
        goto eof;
    } else if (result < 0) {
        lav_error("avcodec_receive_packet", result);
        return STREAM_ERROR;
    }

    result = av_interleaved_write_frame(self->out_ctx, enc_packet);
    if (result < 0) {
        lav_error("av_interleaved_write_frame", result);
        return STREAM_ERROR;
    }

    return STREAM_OK;

eof:
    return STREAM_EOF;
}

static int internal_next(
    struct AudioStream *self,
    AVPacket *in_packet,
    AVPacket *enc_packet,
    AVFrame *in_frame,
    AVFrame *out_frame
) {
    int result;

    if (!self->started) {
        result = avformat_write_header(self->out_ctx, NULL);
        if (result < 0) {
            lav_error("avformat_write_header", result);
            return result;
        }

        self->started = 1;

        return STREAM_OK;
    }

    while (1) {
        result = encode_mux(self, enc_packet);
        av_packet_unref(enc_packet);
        if (result == STREAM_ERROR) {
            goto finish;
        } else if (result == STREAM_OK) {
            return STREAM_OK;
        } else if (result == STREAM_EOF) {
            goto finish;
        }

        result = resample_encode(self, out_frame);
        av_frame_unref(out_frame);
        if (result == STREAM_ERROR) {
            goto finish;
        } else if (result == STREAM_OK || result == STREAM_EOF) {
            continue;
        }

        result = decode_resample(self, in_frame);
        av_frame_unref(in_frame);
        if (result == STREAM_ERROR) {
            goto finish;
        } else if (result == STREAM_OK || result == STREAM_EOF) {
            continue;
        }

        while (1) {
            result = demux_decode(self, in_packet);
            av_packet_unref(in_packet);
            if (result == STREAM_ERROR) {
                goto finish;
            } else if (result == STREAM_OK || result == STREAM_EOF) {
                break;
            }
        }
    }

finish:
    if (result == STREAM_EOF) {
        result = av_write_trailer(self->out_ctx);
        if (result < 0) {
            lav_error("av_write_trailer", result);
            result = STREAM_ERROR;
        }

        result = STREAM_EOF;
    }

    return result;
}

int audio_stream_next(
    struct AudioStream *self,
    void *write_opaque,
    int (*write_callback)(void *opaque, uint8_t *buf, int len)
) {
    self->write_opaque = write_opaque;
    self->write_callback = write_callback;

    AVPacket in_packet = { .data = NULL, .size = 0 };
    av_init_packet(&in_packet);

    AVPacket enc_packet = { .data = NULL, .size = 0 };
    av_init_packet(&enc_packet);

    AVFrame *in_frame = av_frame_alloc();
    AVFrame *out_frame = av_frame_alloc();

    int result = internal_next(self, &in_packet, &enc_packet, in_frame, out_frame);

    av_packet_unref(&enc_packet);
    av_frame_free(&out_frame);
    av_frame_free(&in_frame);
    av_packet_unref(&in_packet);

    return result;
}

void audio_stream_close(struct AudioStream *self) {
    avfilter_graph_free(&self->filter_graph);
    if (self->out_ioctx) {
        av_free(self->out_ioctx->buffer);
    }
    av_free(self->out_ioctx);
    avcodec_close(self->enc_ctx);
    avcodec_free_context(&self->enc_ctx);
    avcodec_close(self->dec_ctx);
    avcodec_free_context(&self->dec_ctx);
    avformat_free_context(self->out_ctx);
    avformat_close_input(&self->in_ctx);
    free(self);
}

