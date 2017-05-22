#![allow(dead_code)]
use core;
use super::histogram::CostAccessors;
use super::super::alloc::SliceWrapper;

use super::util::{brotli_max_uint32_t, FastLog2, floatX, kLog2Table};

use super::vectorization::{v256,v128,v256i,v128i, Mem256i, sum8};

static kCopyBase: [u32; 24] = [2, 3, 4, 5, 6, 7, 8, 9, 10, 12, 14, 18, 22, 30, 38, 54, 70,
                                   102, 134, 198, 326, 582, 1094, 2118];

static kCopyExtra: [u32; 24] = [0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 7, 8,
                                    9, 10, 24];

static kBrotliMinWindowBits: i32 = 10i32;

static kBrotliMaxWindowBits: i32 = 24i32;



pub fn ShannonEntropy(mut population: &[u32], size: usize, mut total: &mut usize) -> super::util::floatX {
  let mut sum: usize = 0usize;
  let mut retval: super::util::floatX = 0i32 as super::util::floatX;
  population = &population[..(size as usize)];
  let mut p: usize;
  let mut odd_number_of_elements_left: i32 = 0i32;
  if size & 1usize != 0 {
    odd_number_of_elements_left = 1i32;
  }
  while population.len() != 0 {
    if odd_number_of_elements_left == 0 {
      p = population[0] as usize;
      population = &population[1..];
      sum = sum.wrapping_add(p);
      retval = retval - p as super::util::floatX * FastLog2(p as u64);
    }
    odd_number_of_elements_left = 0i32;
    p = population[0] as usize;
    population = &population[1..];
    sum = sum.wrapping_add(p);
    retval = retval - p as super::util::floatX * FastLog2(p as u64);
  }
  if sum != 0 {
    retval = retval + sum as super::util::floatX * FastLog2(sum as u64);
  }
  *total = sum;
  retval
}

pub fn BitsEntropy(population: &[u32], size: usize) -> super::util::floatX {
  let mut sum: usize = 0;
  let mut retval: super::util::floatX = ShannonEntropy(population, size, &mut sum);
  if retval < sum as super::util::floatX {
    retval = sum as super::util::floatX;
  }
  retval
}

const BROTLI_REPEAT_ZERO_CODE_LENGTH: usize = 17;
const BROTLI_CODE_LENGTH_CODES: usize = BROTLI_REPEAT_ZERO_CODE_LENGTH + 1;
/*
use std::io::{self, Error, ErrorKind, Read, Write};

macro_rules! println_stderr(
    ($($val:tt)*) => { {
        writeln!(&mut ::std::io::stderr(), $($val)*).unwrap();
    } }
);
*/
fn CostComputation<T:SliceWrapper<Mem256i> >(depth_histo: &mut [u32;BROTLI_CODE_LENGTH_CODES],
                   nnz_data: &T,
                   nnz: usize,
                   total_count: super::util::floatX,
                   log2total: super::util::floatX) -> super::util::floatX {
    let mut bits : super::util::floatX = 0.0 as super::util::floatX;
    if (false) {
      let mut max_depth : usize = 1;
      for i in 0..nnz {
          // Compute -log2(P(symbol)) = -log2(count(symbol)/total_count) =
          //                            = log2(total_count) - log2(count(symbol))
         let element = nnz_data.slice()[i>>3].0[i&7];
         let log2p = log2total - FastLog2(element as u64);
         // Approximate the bit depth by round(-log2(P(symbol)))
         let depth = core::cmp::min((log2p + 0.5) as u8, 15u8);
         bits += element as super::util::floatX * log2p;
         if (depth as usize > max_depth) {
            max_depth = depth as usize;
         }
         depth_histo[depth as usize] += 1;
      }

      // Add the estimated encoding cost of the code length code histogram.
      bits += (18 + 2 * max_depth) as super::util::floatX;
      // Add the entropy of the code length code histogram.
      bits += BitsEntropy(depth_histo, BROTLI_CODE_LENGTH_CODES);
      //println_stderr!("{:?} {:?}", &depth_histo[..], bits);
      return bits;
    }
    if (true) {
      let ff = v256i::set1(0xff);
      let fifteen = v256i::set1(0xf);
        let mut ymm_max_depth = v256i::set1(1);
        let ymm_log2total = v256::set1(log2total);
        let mut ymm_bits = v256::set1(0.0 as super::util::floatX);
      for nnz_data_vec in nnz_data.slice()[..(nnz >> 3)].iter() {
          let elements = v256i::new(nnz_data_vec);
          let elements256 = and256i!(elements, ff);
          let log2elements = v256{
              lo:v128{x0:kLog2Table[elements256.lo.x0 as usize],
                      x1:kLog2Table[elements256.lo.x1 as usize],
                      x2:kLog2Table[elements256.lo.x2 as usize],
                      x3:kLog2Table[elements256.lo.x3 as usize]},
              hi:v128{x0:kLog2Table[elements256.hi.x0 as usize],
                      x1:kLog2Table[elements256.hi.x1 as usize],
                      x2:kLog2Table[elements256.hi.x2 as usize],
                      x3:kLog2Table[elements256.hi.x3 as usize]}};
          let log2p = sub256!(ymm_log2total, log2elements);
          let elementsf = v256::from(elements);
          let cur_bit = mul256!(elementsf, log2p);
          let log2pi = v256i::from(log2p);
          let depth = min256i!(log2pi, fifteen);
          ymm_bits = add256!(ymm_bits, cur_bit);
          ymm_max_depth = max256i!(depth, ymm_max_depth);
          depth_histo[depth.lo.x0 as usize] += 1;
          depth_histo[depth.lo.x1 as usize] += 1;
          depth_histo[depth.lo.x2 as usize] += 1;
          depth_histo[depth.lo.x3 as usize] += 1;
          depth_histo[depth.hi.x0 as usize] += 1;
          depth_histo[depth.hi.x1 as usize] += 1;
          depth_histo[depth.hi.x2 as usize] += 1;
          depth_histo[depth.hi.x3 as usize] += 1;
      }
        bits += ymm_bits.lo.x0 +
            ymm_bits.lo.x1 +
            ymm_bits.lo.x2 +
            ymm_bits.lo.x3 +
            ymm_bits.hi.x0 +
            ymm_bits.hi.x1 +
            ymm_bits.hi.x2 +
            ymm_bits.hi.x3;
      let mut max_depth : i32 = 1;
      if (ymm_max_depth.lo.x0 > max_depth){
          max_depth = ymm_max_depth.lo.x0;
      }
      if (ymm_max_depth.lo.x1 > max_depth){
          max_depth = ymm_max_depth.lo.x1;
      }
      if (ymm_max_depth.lo.x2 > max_depth){
          max_depth = ymm_max_depth.lo.x2;
      }
      if (ymm_max_depth.lo.x3 > max_depth){
          max_depth = ymm_max_depth.lo.x3;
      }
      if (ymm_max_depth.hi.x0 > max_depth){
          max_depth = ymm_max_depth.hi.x0;
      }
      if (ymm_max_depth.hi.x1 > max_depth){
          max_depth = ymm_max_depth.hi.x1;
      }
      if (ymm_max_depth.hi.x2 > max_depth){
          max_depth = ymm_max_depth.hi.x2;
      }
      if (ymm_max_depth.hi.x3 > max_depth){
          max_depth = ymm_max_depth.hi.x3;
      }
      for i in ((nnz >> 3)<<3)..nnz {
          let last_vec = nnz_data.slice()[(nnz >> 3)];
          let element = last_vec.0[i&7];
          let log2p = log2total - FastLog2(element as u64);
          // Approximate the bit depth by round(-log2(P(symbol)))
          let depth = core::cmp::min((log2p + 0.5) as u8, 15u8);
          bits += element as super::util::floatX * log2p;
          if (depth as i32 > max_depth) {
             max_depth = depth as i32;
          }
          depth_histo[depth as usize] += 1;          
      }
      // Add the estimated encoding cost of the code length code histogram.
      bits += (18 + 2 * max_depth) as super::util::floatX;
      // Add the entropy of the code length code histogram.
      bits += BitsEntropy(depth_histo, BROTLI_CODE_LENGTH_CODES);
      //println_stderr!("{:?} {:?}", &depth_histo[..], bits);
      return bits;
    }
  let pow2l = v256::setr(
      1.0/*0.7071067811865476*/ as floatX,
      0.3535533905932738 as floatX,
      0.1767766952966369 as floatX,
      0.0883883476483184 as floatX,
      0.0441941738241592 as floatX,
      0.0220970869120796 as floatX,
      0.0110485434560398 as floatX,
      0.0055242717280199 as floatX);
  let pow2h = v256::setr(
      0.0027621358640100 as floatX,
      0.0013810679320050 as floatX,
      0.0006905339660025 as floatX,
      0.0003452669830012 as floatX,
      0.0001726334915006 as floatX,
      0.0000863167457503 as floatX,
      0.0000431583728752 as floatX,
      /*0.0000215791864376f*/0.0 as floatX);
  let ymm_tc = v256::set1(total_count as floatX);
  let search_depthl = v256i::from(mul256!(pow2l, ymm_tc));
  let search_depthh = v256i::from(mul256!(pow2h, ymm_tc));
  let mut suml = v256i::set1(0);
  let mut sumh = v256i::set1(0);
  for nnz_data_vec in nnz_data.slice()[..(nnz>>3)].iter() {
      for sub_data_item in nnz_data_vec.0.iter() {
          let count = v256i::set1(*sub_data_item);
          let cmpl = cmpgt256and1!(count, search_depthl);
          let cmph = cmpgt256and1!(count, search_depthh);
          suml = add256i!(suml, cmpl);
          sumh = add256i!(sumh, cmph);
      }
  }
  let last_element = (nnz>>3);
  for sub_index in (last_element<<3)..nnz {
      let nnz_data_vec = nnz_data.slice()[last_element];
      let count = v256i::set1(nnz_data_vec.0[sub_index & 7]);
      let cmpl = cmpgt256and1!(count, search_depthl);
      let cmph = cmpgt256and1!(count, search_depthh);
      suml = add256i!(suml, cmpl);
      sumh = add256i!(sumh, cmph);
  }
    let mut max_depth : usize = 1;
  // Deal with depth_histo and max_depth
  {
    let cumulative_sum:[Mem256i;2] = [Mem256i::new(suml),
                                      Mem256i::new(sumh)];
    let mut prev = cumulative_sum[0].0[0];
    for j in 1..16 {
      let cur = cumulative_sum[(j&8) >> 3].0[j & 7];
      let delta = cur - prev;
      prev = cur;
      let mut cur = &mut depth_histo[j];
      *cur = (*cur as i32 + delta) as u32; // depth_histo[j] += delta
      if delta != 0 {
         max_depth = j;
      }
    }
  }
  let ymm_log2total = v256::set1(log2total);
  let mut bits_cumulative = v256::set1(0.0 as floatX);
  for nnz_data_item in nnz_data.slice()[..(nnz >> 3)].iter() {
      let counts = v256::from(v256i::new(nnz_data_item));
      let log_counts = logtwo256i!(counts);
      let log2p = sub256!(ymm_log2total, log_counts);
      let tmp = mul256!(counts, log2p);
      bits_cumulative = add256!(bits_cumulative, tmp);
  }
  bits += sum8(bits_cumulative);
  for i in ((nnz >> 3)<<3)..nnz {
      let last_vec = nnz_data.slice()[(nnz >> 3)];
      let last_item = last_vec.0[i&3];
      let log2p = log2total - FastLog2(last_item as u64);
      bits += last_item as super::util::floatX * log2p;
  }

  // Add the estimated encoding cost of the code length code histogram.
  bits += (18 + 2 * max_depth) as super::util::floatX;
  // Add the entropy of the code length code histogram.
  bits += BitsEntropy(depth_histo, BROTLI_CODE_LENGTH_CODES);
  //println_stderr!("{:?} {:?}", depth_histo, bits);
  return bits;
}
use alloc::SliceWrapperMut;

pub fn BrotliPopulationCost<HistogramType:SliceWrapper<u32>+CostAccessors>(
    histogram : &HistogramType
) -> super::util::floatX{
  static kOneSymbolHistogramCost: super::util::floatX = 12i32 as super::util::floatX;
  static kTwoSymbolHistogramCost: super::util::floatX = 20i32 as super::util::floatX;
  static kThreeSymbolHistogramCost: super::util::floatX = 28i32 as super::util::floatX;
  static kFourSymbolHistogramCost: super::util::floatX = 37i32 as super::util::floatX;
  let data_size: usize = (*histogram).slice().len();
  let mut count: i32 = 0i32;
  let mut s: [usize; 5] = [0; 5];

  let mut bits: super::util::floatX = 0.0 as super::util::floatX;
  let mut i: usize;
  if (*histogram).total_count() == 0usize {
    return kOneSymbolHistogramCost;
  }
  i = 0usize;
  'break1: while i < data_size {
    {
      if (*histogram).slice()[i] > 0u32 {
        s[count as (usize)] = i;
        count = count + 1;
        if count > 4i32 {
          {
            break 'break1;
          }
        }
      }
    }
    i = i.wrapping_add(1 as (usize));
  }
  if count == 1i32 {
    return kOneSymbolHistogramCost;
  }
  if count == 2i32 {
    return kTwoSymbolHistogramCost + (*histogram).total_count() as super::util::floatX;
  }
  if count == 3i32 {
    let histo0: u32 = (*histogram).slice()[s[0usize]];
    let histo1: u32 = (*histogram).slice()[s[1usize]];
    let histo2: u32 = (*histogram).slice()[s[2usize]];
    let histomax: u32 = brotli_max_uint32_t(histo0, brotli_max_uint32_t(histo1, histo2));
    return kThreeSymbolHistogramCost +
           (2u32).wrapping_mul(histo0.wrapping_add(histo1).wrapping_add(histo2)) as super::util::floatX -
           histomax as super::util::floatX;
  }
  if count == 4i32 {
    let mut histo: [u32; 4] = [0; 4];
    let h23: u32;
    let histomax: u32;
    i = 0usize;
    while i < 4usize {
      {
        histo[i] = (*histogram).slice()[s[i]];
      }
      i = i.wrapping_add(1 as (usize));
    }
    i = 0usize;
    while i < 4usize {
      {
        let mut j: usize;
        j = i.wrapping_add(1usize);
        while j < 4usize {
          {
            if histo[j] > histo[i] {
              let mut __brotli_swap_tmp: u32 = histo[j];
              histo[j] = histo[i];
              histo[i] = __brotli_swap_tmp;
            }
          }
          j = j.wrapping_add(1 as (usize));
        }
      }
      i = i.wrapping_add(1 as (usize));
    }
    h23 = histo[2usize].wrapping_add(histo[3usize]);
    histomax = brotli_max_uint32_t(h23, histo[0usize]);
    return kFourSymbolHistogramCost + (3u32).wrapping_mul(h23) as super::util::floatX +
           (2u32).wrapping_mul(histo[0usize].wrapping_add(histo[1usize])) as super::util::floatX -
           histomax as super::util::floatX;
  }
  {
    let mut nnz: usize = 0;
    let mut depth_histo: [u32; 18] = [0u32; 18];
    let mut nnz_data = HistogramType::make_nnz_storage();
    let total_count = (*histogram).total_count() as super::util::floatX;
    let log2total = FastLog2((*histogram).total_count() as u64);
    i = 0usize;
    while i < data_size {
      if (*histogram).slice()[i] > 0u32 {
        nnz_data.slice_mut()[nnz>>3].0[nnz&7] = histogram.slice()[i] as i32;
        i += 1;
        nnz += 1;
      } else {
        let mut reps: u32 = 1;
        for hd in (*histogram).slice()[i+1..(data_size as usize)].iter() {
            if *hd != 0 {
               break
            }
            reps += 1
        }
        i += reps as usize;
        if i == data_size {
          {
            break;
          }
        }
        if reps < 3 {
          depth_histo[0] += reps
        } else {
          reps -= 2;
          let mut depth_histo_adds : u32 = 0;
          while reps > 0u32 {
            depth_histo_adds += 1;
            bits = bits + 3i32 as super::util::floatX;
            reps = reps >> 3i32;
          }
          depth_histo[BROTLI_REPEAT_ZERO_CODE_LENGTH] += depth_histo_adds;
        }
      }
    }
    bits += CostComputation(&mut depth_histo, &nnz_data, nnz, total_count, log2total);
  }
  bits
}
/*
fn HistogramDataSizeCommand() -> usize {
    704i32 as (usize)
}*/


/*
fn HistogramDataSizeDistance() -> usize {
    520i32 as (usize)
}
*/
