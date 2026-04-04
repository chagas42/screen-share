use anyhow::{Context, Result};
use capture::Frame;
use ffmpeg_next as ffmpeg;
use ffmpeg::codec::Id;
use ffmpeg::codec::context::Context as CodecContext;
use ffmpeg::format::Pixel;
use ffmpeg::frame;
use ffmpeg::software::scaling::{self, Flags};
use ffmpeg::{Dictionary, Packet, Rational};

// ─── Encoder ─────────────────────────────────────────────────────────────────

pub struct Encoder {
    inner: ffmpeg::encoder::Video,
    sws: scaling::Context,
    pts: i64,
}

pub trait Codec {
    fn new(width: u32, height: u32) -> Result<Self>
    where
        Self: Sized;
    fn encode(&mut self, frame: &Frame) -> Result<Option<Vec<u8>>>;
}

impl Codec for Encoder {
    fn new(width: u32, height: u32) -> Result<Self> {
        ffmpeg::init().context("ffmpeg::init()")?;

        let codec = ffmpeg::encoder::find(Id::H264)
            .context("libx264 nao encontrado — ffmpeg compilado com --enable-libx264?")?;

        let mut ctx = CodecContext::new_with_codec(codec)
            .encoder()
            .video()
            .context("encoder::video()")?;

        ctx.set_width(width);
        ctx.set_height(height);
        ctx.set_format(Pixel::YUV420P);
        ctx.set_time_base(Rational(1, 1000)); // timestamps em ms
        ctx.set_gop(120); // keyframe a cada 2s a 60fps
        ctx.set_max_b_frames(0); // sem B-frames — latencia minima

        let mut opts = Dictionary::new();
        opts.set("preset", "ultrafast");
        opts.set("tune", "zerolatency");

        let inner = ctx
            .open_with(opts)
            .context("abrir encoder H264")?;

        let sws = scaling::Context::get(
            Pixel::BGRA,
            width,
            height,
            Pixel::YUV420P,
            width,
            height,
            Flags::BILINEAR,
        )
        .context("sws BGRA->YUV420P")?;

        Ok(Self { inner, sws, pts: 0 })
    }

    /// Retorna NAL units prontos para transmissao/decode.
    /// Pode retornar `Ok(None)` enquanto o encoder buferiza frames iniciais.
    fn encode(&mut self, frame: &Frame) -> Result<Option<Vec<u8>>> {
        let w = frame.width;
        let h = frame.height;

        let mut src = frame::Video::new(Pixel::BGRA, w, h);
        src.data_mut(0).copy_from_slice(&frame.data);
        src.set_pts(Some(self.pts));
        self.pts += 1;

        let mut yuv = frame::Video::new(Pixel::YUV420P, w, h);
        self.sws.run(&src, &mut yuv).context("sws_scale BGRA->YUV420P")?;
        yuv.set_pts(src.pts());

        self.inner.send_frame(&yuv).context("send_frame")?;

        let mut pkt = Packet::empty();
        match self.inner.receive_packet(&mut pkt) {
            Ok(()) => Ok(Some(pkt.data().unwrap_or(&[]).to_vec())),
            Err(ffmpeg::Error::Other { errno })
                if errno == ffmpeg::ffi::AVERROR(libc::EAGAIN) =>
            {
                Ok(None)
            }
            Err(ffmpeg::Error::Eof) => Ok(None),
            Err(e) => Err(e).context("receive_packet"),
        }
    }
}

// ─── Decoder ─────────────────────────────────────────────────────────────────

pub struct Decoder {
    inner: ffmpeg::decoder::Video,
    sws: Option<scaling::Context>,
}

impl Decoder {
    pub fn new() -> Result<Self> {
        ffmpeg::init().context("ffmpeg::init()")?;

        let codec =
            ffmpeg::decoder::find(Id::H264).context("decoder H264 nao encontrado")?;

        let inner = CodecContext::new_with_codec(codec)
            .decoder()
            .video()
            .context("decoder.video()")?;

        Ok(Self { inner, sws: None })
    }

    /// Retorna Frame BGRA ou `Ok(None)` se o decoder ainda nao produziu output.
    pub fn decode(&mut self, nal_data: &[u8]) -> Result<Option<Frame>> {
        let pkt = Packet::copy(nal_data);
        self.inner.send_packet(&pkt).context("send_packet")?;

        let mut yuv = frame::Video::empty();
        match self.inner.receive_frame(&mut yuv) {
            Err(ffmpeg::Error::Other { errno })
                if errno == ffmpeg::ffi::AVERROR(libc::EAGAIN) =>
            {
                return Ok(None);
            }
            Err(ffmpeg::Error::Eof) => return Ok(None),
            Err(e) => return Err(e).context("receive_frame"),
            Ok(()) => {}
        }

        let w = yuv.width();
        let h = yuv.height();

        let sws = match &mut self.sws {
            Some(s) => s,
            None => {
                self.sws = Some(
                    scaling::Context::get(
                        Pixel::YUV420P,
                        w,
                        h,
                        Pixel::BGRA,
                        w,
                        h,
                        Flags::BILINEAR,
                    )
                    .context("sws YUV420P->BGRA")?,
                );
                self.sws.as_mut().unwrap()
            }
        };

        let mut bgra = frame::Video::new(Pixel::BGRA, w, h);
        sws.run(&yuv, &mut bgra).context("sws_scale YUV420P->BGRA")?;

        Ok(Some(Frame {
            data: bgra.data(0).to_vec(),
            width: w,
            height: h,
        }))
    }
}
