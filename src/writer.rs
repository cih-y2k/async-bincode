use bincode;
use byteorder::{NetworkEndian, WriteBytesExt};
use serde::Serialize;
use std::marker::PhantomData;
use tokio;
use tokio::prelude::*;

/// A wrapper around an asynchronous sink that accepts, serializes, and sends bincode-encoded
/// values.
///
/// To use, provide a writer that implements `futures::AsyncWrite`, and then use `futures::Sink`
/// to send values.
///
/// Note that an `AsyncBincodeWriter` must be of the type [`AsyncDestination`] in order to be
/// compatible with an [`AsyncBincodeReader`] on the remote end (recall that it requires the
/// serialized size prefixed to the serialized data). The default is [`SyncDestination`], but these
/// can be easily toggled between using [`AsyncBincodeWriter::for_async`].
#[derive(Debug)]
pub struct AsyncBincodeWriter<W, T, D> {
    writer: W,
    pub(crate) written: usize,
    pub(crate) buffer: Vec<u8>,
    from: PhantomData<T>,
    dest: PhantomData<D>,
}

impl<W, T> Default for AsyncBincodeWriter<W, T, SyncDestination>
where
    W: Default,
{
    fn default() -> Self {
        Self::from(W::default())
    }
}

impl<W, T, D> AsyncBincodeWriter<W, T, D> {
    /// Gets a reference to the underlying writer.
    ///
    /// It is inadvisable to directly write to the underlying writer.
    pub fn get_ref(&self) -> &W {
        &self.writer
    }

    /// Gets a mutable reference to the underlying writer.
    ///
    /// It is inadvisable to directly write to the underlying writer.
    pub fn get_mut(&mut self) -> &mut W {
        &mut self.writer
    }

    /// Unwraps this `AsyncBincodeWriter`, returning the underlying writer.
    ///
    /// Note that any leftover serialized data that has not yet been sent is lost.
    pub fn into_inner(self) -> W {
        self.writer
    }
}

impl<W, T> From<W> for AsyncBincodeWriter<W, T, SyncDestination> {
    fn from(writer: W) -> Self {
        AsyncBincodeWriter {
            buffer: Vec::new(),
            writer,
            written: 0,
            from: PhantomData,
            dest: PhantomData,
        }
    }
}

impl<W, T> AsyncBincodeWriter<W, T, SyncDestination> {
    /// Make this writer include the serialized data's size before each serialized value.
    ///
    /// This is necessary for compatability with [`AsyncBincodeReader`].
    pub fn for_async(self) -> AsyncBincodeWriter<W, T, AsyncDestination> {
        self.make_for()
    }
}

impl<W, T, D> AsyncBincodeWriter<W, T, D> {
    pub(crate) fn make_for<D2>(self) -> AsyncBincodeWriter<W, T, D2> {
        AsyncBincodeWriter {
            buffer: self.buffer,
            writer: self.writer,
            written: self.written,
            from: self.from,
            dest: PhantomData,
        }
    }
}

impl<W, T> AsyncBincodeWriter<W, T, AsyncDestination> {
    /// Make this writer only send bincode-encoded values.
    ///
    /// This is necessary for compatability with stock `bincode` receivers.
    pub fn for_sync(self) -> AsyncBincodeWriter<W, T, SyncDestination> {
        self.make_for()
    }
}

/// A marker that indicates that the wrapping type is compatible with `AsyncBincodeReader`.
#[derive(Debug)]
pub struct AsyncDestination;

/// A marker that indicates that the wrapping type is compatible with stock `bincode` receivers.
#[derive(Debug)]
pub struct SyncDestination;

#[doc(hidden)]
pub trait BincodeWriterFor<T> {
    fn append(&mut self, item: T) -> Result<(), bincode::Error>;
}

impl<W, T> BincodeWriterFor<T> for AsyncBincodeWriter<W, T, AsyncDestination>
where
    T: Serialize,
{
    fn append(&mut self, item: T) -> Result<(), bincode::Error> {
        let mut c = bincode::config();
        let c = c.limit(u32::max_value() as u64);
        let size = c.serialized_size(&item)? as u32;
        self.buffer.write_u32::<NetworkEndian>(size)?;
        c.serialize_into(&mut self.buffer, &item)
    }
}

impl<W, T> BincodeWriterFor<T> for AsyncBincodeWriter<W, T, SyncDestination>
where
    T: Serialize,
{
    fn append(&mut self, item: T) -> Result<(), bincode::Error> {
        bincode::serialize_into(&mut self.buffer, &item)
    }
}

impl<W, T, D> Sink for AsyncBincodeWriter<W, T, D>
where
    T: Serialize,
    W: tokio::io::AsyncWrite,
    Self: BincodeWriterFor<T>,
{
    type SinkItem = T;
    type SinkError = bincode::Error;

    fn start_send(
        &mut self,
        item: Self::SinkItem,
    ) -> Result<AsyncSink<Self::SinkItem>, Self::SinkError> {
        if self.buffer.is_empty() {
            // NOTE: in theory we could have a short-circuit here that tries to have bincode write
            // directly into self.writer. this would be way more efficient in the common case as we
            // don't have to do the extra buffering. the idea would be to serialize fist, and *if*
            // it errors, see how many bytes were written, serialize again into a Vec, and then
            // keep only the bytes following the number that were written in our buffer.
            // unfortunately, bincode will not tell us that number at the moment, and instead just
            // fail.
        }

        self.append(item)?;
        Ok(AsyncSink::Ready)
    }

    fn poll_complete(&mut self) -> Result<Async<()>, Self::SinkError> {
        // write stuff out if we need to
        while self.written != self.buffer.len() {
            let n = try_ready!(self.writer.poll_write(&self.buffer[self.written..]));
            self.written += n;
        }

        // we have to flush before we're really done
        self.buffer.clear();
        self.written = 0;
        self.writer.poll_flush().map_err(bincode::Error::from)
    }
}
