mod integration_tests;
mod tests;
extern crate brotli;
extern crate core;
#[allow(unused_imports)]
#[macro_use]
extern crate alloc_no_stdlib;
use brotli::CustomRead;
use core::ops;
pub struct Rebox<T> {
  b: Box<[T]>,
}

impl<T> core::default::Default for Rebox<T> {
  fn default() -> Self {
    let v: Vec<T> = Vec::new();
    let b = v.into_boxed_slice();
    Rebox::<T> { b: b }
  }
}

impl<T> ops::Index<usize> for Rebox<T> {
  type Output = T;
  fn index(&self, index: usize) -> &T {
    &(*self.b)[index]
  }
}

impl<T> ops::IndexMut<usize> for Rebox<T> {
  fn index_mut(&mut self, index: usize) -> &mut T {
    &mut (*self.b)[index]
  }
}

impl<T> alloc_no_stdlib::SliceWrapper<T> for Rebox<T> {
  fn slice(&self) -> &[T] {
    &*self.b
  }
}

impl<T> alloc_no_stdlib::SliceWrapperMut<T> for Rebox<T> {
  fn slice_mut(&mut self) -> &mut [T] {
    &mut *self.b
  }
}

pub struct HeapAllocator<T: core::clone::Clone> {
  pub default_value: T,
}

#[cfg(not(feature="unsafe"))]
impl<T: core::clone::Clone> alloc_no_stdlib::Allocator<T> for HeapAllocator<T> {
  type AllocatedMemory = Rebox<T>;
  fn alloc_cell(self: &mut HeapAllocator<T>, len: usize) -> Rebox<T> {
    let v: Vec<T> = vec![self.default_value.clone();len];
    let b = v.into_boxed_slice();
    Rebox::<T> { b: b }
  }
  fn free_cell(self: &mut HeapAllocator<T>, _data: Rebox<T>) {}
}

#[cfg(feature="unsafe")]
impl<T: core::clone::Clone> alloc_no_stdlib::Allocator<T> for HeapAllocator<T> {
  type AllocatedMemory = Rebox<T>;
  fn alloc_cell(self: &mut HeapAllocator<T>, len: usize) -> Rebox<T> {
    let mut v: Vec<T> = Vec::with_capacity(len);
    unsafe {
      v.set_len(len);
    }
    let b = v.into_boxed_slice();
    Rebox::<T> { b: b }
  }
  fn free_cell(self: &mut HeapAllocator<T>, _data: Rebox<T>) {}
}


#[allow(unused_imports)]
use alloc_no_stdlib::{SliceWrapper, SliceWrapperMut, StackAllocator, AllocatedStackMemory,
                      Allocator, bzero};
use brotli::HuffmanCode;

use std::env;

use std::fs::File;

use std::io::{self, Error, ErrorKind, Read, Write};

use std::path::Path;


// declare_stack_allocator_struct!(MemPool, 4096, global);



struct IoWriterWrapper<'a, OutputType: Write + 'a>(&'a mut OutputType);


struct IoReaderWrapper<'a, OutputType: Read + 'a>(&'a mut OutputType);

impl<'a, OutputType: Write> brotli::CustomWrite<io::Error> for IoWriterWrapper<'a, OutputType> {
  fn write(self: &mut Self, buf: &[u8]) -> Result<usize, io::Error> {
    loop {
      match self.0.write(buf) {
        Err(e) => {
          match e.kind() {
            ErrorKind::Interrupted => continue,
            _ => return Err(e),
          }
        }
        Ok(cur_written) => return Ok(cur_written),
      }
    }
  }
}


impl<'a, InputType: Read> brotli::CustomRead<io::Error> for IoReaderWrapper<'a, InputType> {
  fn read(self: &mut Self, buf: &mut [u8]) -> Result<usize, io::Error> {
    loop {
      match self.0.read(buf) {
        Err(e) => {
          match e.kind() {
            ErrorKind::Interrupted => continue,
            _ => return Err(e),
          }
        }
        Ok(cur_read) => return Ok(cur_read),
      }
    }
  }
}

struct IntoIoReader<OutputType: Read>(OutputType);

impl<InputType: Read> brotli::CustomRead<io::Error> for IntoIoReader<InputType> {
  fn read(self: &mut Self, buf: &mut [u8]) -> Result<usize, io::Error> {
    loop {
      match self.0.read(buf) {
        Err(e) => {
          match e.kind() {
            ErrorKind::Interrupted => continue,
            _ => return Err(e),
          }
        }
        Ok(cur_read) => return Ok(cur_read),
      }
    }
  }
}
#[cfg(not(feature="seccomp"))]
pub fn decompress<InputType, OutputType>(r: &mut InputType,
                                         mut w: &mut OutputType,
                                         buffer_size: usize)
                                         -> Result<(), io::Error>
  where InputType: Read,
        OutputType: Write
{
  let mut alloc_u8 = HeapAllocator::<u8> { default_value: 0 };
  let mut input_buffer = alloc_u8.alloc_cell(buffer_size);
  let mut output_buffer = alloc_u8.alloc_cell(buffer_size);
  brotli::BrotliDecompressCustomIo(&mut IoReaderWrapper::<InputType>(r),
                                   &mut IoWriterWrapper::<OutputType>(w),
                                   input_buffer.slice_mut(),
                                   output_buffer.slice_mut(),
                                   alloc_u8,
                                   HeapAllocator::<u32> { default_value: 0 },
                                   HeapAllocator::<HuffmanCode> {
                                     default_value: HuffmanCode::default(),
                                   },
                                   Error::new(ErrorKind::UnexpectedEof, "Unexpected EOF"))
}
#[cfg(feature="seccomp")]
extern "C" {
  fn calloc(n_elem: usize, el_size: usize) -> *mut u8;
  fn free(ptr: *mut u8);
  fn syscall(value: i32) -> i32;
  fn prctl(operation: i32, flags: u32) -> i32;
}
#[cfg(feature="seccomp")]
const PR_SET_SECCOMP: i32 = 22;
#[cfg(feature="seccomp")]
const SECCOMP_MODE_STRICT: u32 = 1;

#[cfg(feature="seccomp")]
declare_stack_allocator_struct!(CallocAllocatedFreelist, 8192, calloc);

#[cfg(feature="seccomp")]
pub fn decompress<InputType, OutputType>(r: &mut InputType,
                                         mut w: &mut OutputType,
                                         buffer_size: usize)
                                         -> Result<(), io::Error>
  where InputType: Read,
        OutputType: Write
{

  let mut u8_buffer =
    unsafe { define_allocator_memory_pool!(4, u8, [0; 1024 * 1024 * 200], calloc) };
  let mut u32_buffer = unsafe { define_allocator_memory_pool!(4, u32, [0; 16384], calloc) };
  let mut hc_buffer =
    unsafe { define_allocator_memory_pool!(4, HuffmanCode, [0; 1024 * 1024 * 16], calloc) };
  let mut alloc_u8 = CallocAllocatedFreelist::<u8>::new_allocator(u8_buffer.data, bzero);
  let alloc_u32 = CallocAllocatedFreelist::<u32>::new_allocator(u32_buffer.data, bzero);
  let alloc_hc = CallocAllocatedFreelist::<HuffmanCode>::new_allocator(hc_buffer.data, bzero);
  let ret = unsafe { prctl(PR_SET_SECCOMP, SECCOMP_MODE_STRICT) };
  if ret != 0 {
    panic!("Unable to activate seccomp");
  }
  match brotli::BrotliDecompressCustomIo(&mut IoReaderWrapper::<InputType>(r),
                                         &mut IoWriterWrapper::<OutputType>(w),
                                         &mut alloc_u8.alloc_cell(buffer_size).slice_mut(),
                                         &mut alloc_u8.alloc_cell(buffer_size).slice_mut(),
                                         alloc_u8,
                                         alloc_u32,
                                         alloc_hc,
                                         Error::new(ErrorKind::UnexpectedEof, "Unexpected EOF")) {
    Err(e) => Err(e),
    Ok(()) => {
        unsafe{syscall(60);};
        unreachable!()
      }
  }
}





// This decompressor is defined unconditionally on whether no-stdlib is defined
// so we can exercise the code in any case
pub struct BrotliDecompressor<R: Read>(brotli::DecompressorCustomIo<io::Error,
                                                                    IntoIoReader<R>,
                                                                    Rebox<u8>,
                                                                    HeapAllocator<u8>,
                                                                    HeapAllocator<u32>,
                                                                    HeapAllocator<HuffmanCode>>);



impl<R: Read> BrotliDecompressor<R> {
  pub fn new(r: R, buffer_size: usize) -> Self {
    let mut alloc_u8 = HeapAllocator::<u8> { default_value: 0 };
    let buffer = alloc_u8.alloc_cell(buffer_size);
    let alloc_u32 = HeapAllocator::<u32> { default_value: 0 };
    let alloc_hc = HeapAllocator::<HuffmanCode> { default_value: HuffmanCode::default() };
    BrotliDecompressor::<R>(
          brotli::DecompressorCustomIo::<Error,
                                 IntoIoReader<R>,
                                 Rebox<u8>,
                                 HeapAllocator<u8>, HeapAllocator<u32>, HeapAllocator<HuffmanCode> >
                                 ::new(IntoIoReader::<R>(r),
                                                         buffer,
                                                         alloc_u8, alloc_u32, alloc_hc,
                                                         io::Error::new(ErrorKind::InvalidData,
                                                                        "Invalid Data")))
  }
}

impl<R: Read> Read for BrotliDecompressor<R> {
  fn read(&mut self, mut buf: &mut [u8]) -> Result<usize, Error> {
    self.0.read(buf)
  }
}

#[cfg(test)]
fn writeln0<OutputType: Write>(strm: &mut OutputType,
                               data: &str)
                               -> core::result::Result<(), io::Error> {
  writeln!(strm, "{:}", data)
}
#[cfg(test)]
fn writeln_time<OutputType: Write>(strm: &mut OutputType,
                                   data: &str,
                                   v0: u64,
                                   v1: u64,
                                   v2: u32)
                                   -> core::result::Result<(), io::Error> {
  writeln!(strm, "{:} {:} {:}.{:09}", v0, data, v1, v2)
}

fn main() {
  let mut q: u32 = 9;
  let mut lgwin: u32 = 22;
  let mut compress = false;
  if env::args_os().len() > 1 {
    let mut first = true;
    let mut found_file = false;
    for argument in env::args() {
      if first {
        first = false;
        continue;
      }
      if argument == "-d" {
        continue;
      }
      if argument == "-0" {
        q = 0;
        lgwin = 10;
        continue;
      }
      if argument == "-1" {
        q = 1;
        lgwin = 10;
        continue;
      }
      if argument == "-2" {
        q = 2;
        lgwin = 12;
        continue;
      }
      if argument == "-3" {
        q = 3;
        lgwin = 14;
        continue;
      }
      if argument == "-4" {
        q = 4;
        lgwin = 16;
        continue;
      }
      if argument == "-5" {
        q = 5;
        lgwin = 18;
        continue;
      }
      if argument == "-6" {
        q = 6;
        lgwin = 19;
        continue;
      }
      if argument == "-7" {
        q = 7;
        lgwin = 20;
        continue;
      }
      if argument == "-8" {
        q = 8;
        lgwin = 21;
        continue;
      }
      if argument == "-9" {
        q = 9;
        lgwin = 22;
        continue;
      }
      if argument == "-c" {
        compress = true;
        continue;
      }
      let mut input = match File::open(&Path::new(&argument)) {
        Err(why) => panic!("couldn't open {}: {:?}", argument, why),
        Ok(file) => file,
      };
      found_file = true;
      let oa = argument + ".original";
      let mut output = match File::create(&Path::new(&oa)) {
        Err(why) => panic!("couldn't open file for writing: {:} {:?}", oa, why),
        Ok(file) => file,
      };
      if compress {
        match brotli::BrotliCompress(&mut input, &mut output, q, lgwin) {
          Ok(_) => {}
          Err(e) => panic!("Error {:?}", e),
        }
      } else {
        match decompress(&mut input, &mut output, 65536) {
          Ok(_) => {}
          Err(e) => panic!("Error {:?}", e),
        }
      }
      drop(output);
      drop(input);
    }
    if !found_file {
      if compress {
        match brotli::BrotliCompress(&mut io::stdin(), &mut io::stdout(), q, lgwin) {
          Ok(_) => return,
          Err(e) => panic!("Error {:?}", e),
        }
      } else {
        match decompress(&mut io::stdin(), &mut io::stdout(), 65536) {
          Ok(_) => return,
          Err(e) => panic!("Error {:?}", e),
        }
      }
    }
  } else {
    match decompress(&mut io::stdin(), &mut io::stdout(), 65536) {
      Ok(_) => return,
      Err(e) => panic!("Error {:?}", e),
    }
  }
}
