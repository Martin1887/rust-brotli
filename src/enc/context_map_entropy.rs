use core;
use super::super::alloc;
use super::super::alloc::{SliceWrapper, SliceWrapperMut};
use super::interface;
use super::input_pair::{InputPair, InputReference, InputReferenceMut};
use super::histogram::ContextType;
use super::constants::{kSigned3BitContextLookup, kUTF8ContextLookup};
use super::util::{floatX, FastLog2u16};
use super::find_stride;
use super::weights::{Weights, BLEND_FIXED_POINT_PRECISION};

const NUM_SPEEDS_TO_TRY: usize = 16;
const SPEEDS_TO_SEARCH: [u16; NUM_SPEEDS_TO_TRY]= [0,
                                                   1, 1, 1,
                                                   2,
                                                   4,
                                                   8,
                                                   16,
                                                   16,
                                                   32,
                                                   64,
                                                   128, 128,
                                                   512,
                                                   1664,
                                                   1664,
                                                   ];
const MAXES_TO_SEARCH: [u16; NUM_SPEEDS_TO_TRY] = [32,
                                                   32, 128, 16384,
                                                   1024,
                                                   1024,
                                                   8192,
                                                   48,
                                                   8192,
                                                   4096,
                                                   16384,
                                                   256, 16384,
                                                   16384,
                                                   16384,
                                                   16384,
                                                   ];
const NIBBLE_PRIOR_SIZE: usize = 16 * NUM_SPEEDS_TO_TRY;
// the high nibble, followed by the low nibbles
const CONTEXT_MAP_PRIOR_SIZE: usize = 256 * NIBBLE_PRIOR_SIZE * 17;
const STRIDE_PRIOR_SIZE: usize = 256 * 256 * NIBBLE_PRIOR_SIZE * 2;
#[derive(Clone,Copy, Debug)]
pub struct SpeedAndMax(pub u16, pub u16);

pub fn speed_to_tuple(inp:[SpeedAndMax;2]) -> [(u16,u16);2] {
   [(inp[0].0, inp[0].1), (inp[1].0, inp[1].1)]
}

fn get_stride_cdf_low(data: &mut [u16], stride_prior: u8, cm_prior: usize, high_nibble: u8) -> &mut [u16] {
    let index: usize =  1 + 2 * (cm_prior as usize | ((stride_prior as usize & 0xf) << 8) | ((high_nibble as usize) << 12));
    data.split_at_mut(NUM_SPEEDS_TO_TRY * index << 4).1.split_at_mut(16 * NUM_SPEEDS_TO_TRY).0
}

fn get_stride_cdf_high(data: &mut [u16], stride_prior: u8, cm_prior: usize) -> &mut [u16] {
    let index: usize = 2 * (cm_prior as usize | ((stride_prior as usize) << 8));
    data.split_at_mut(NUM_SPEEDS_TO_TRY * index << 4).1.split_at_mut(16 * NUM_SPEEDS_TO_TRY).0
}

fn get_cm_cdf_low(data: &mut [u16], cm_prior: usize, high_nibble: u8) -> &mut [u16] {
    let index: usize = (high_nibble as usize + 1) + 17 * cm_prior as usize;
    data.split_at_mut(NUM_SPEEDS_TO_TRY * index << 4).1.split_at_mut(16 * NUM_SPEEDS_TO_TRY).0
}

fn get_cm_cdf_high(data: &mut [u16], cm_prior: usize) -> &mut [u16] {
    let index: usize = 17 * cm_prior as usize;
    data.split_at_mut(NUM_SPEEDS_TO_TRY * index << 4).1.split_at_mut(16 * NUM_SPEEDS_TO_TRY).0
}
fn init_cdfs(cdfs: &mut [u16]) {
    assert_eq!(cdfs.len() % (16 * NUM_SPEEDS_TO_TRY), 0);
    let mut total_index = 0usize;
    let len = cdfs.len();
    loop {
        for cdf_index in 0..16 {
            let mut vec = cdfs.split_at_mut(total_index).1.split_at_mut(NUM_SPEEDS_TO_TRY).0;
            for item in vec {
                *item = 4 + 4 * cdf_index as u16;
            }
            total_index += NUM_SPEEDS_TO_TRY;
        }
        if total_index == len {
            break;
        }
    }
}
fn compute_combined_cost(singleton_cost: &mut [floatX;NUM_SPEEDS_TO_TRY],
                cdfs: &[u16],
                mixing_cdf: [u16;16],
                nibble_u8: u8,
                _weights: &mut [Weights; NUM_SPEEDS_TO_TRY]) {
    assert_eq!(cdfs.len(), 16 * NUM_SPEEDS_TO_TRY);
    let nibble = nibble_u8 as usize & 0xf;
    let mut stride_pdf = [0u16; NUM_SPEEDS_TO_TRY];
    stride_pdf.clone_from_slice(cdfs.split_at(NUM_SPEEDS_TO_TRY * nibble).1.split_at(NUM_SPEEDS_TO_TRY).0);
    let mut cm_pdf:u16 = mixing_cdf[nibble];
    if nibble_u8 != 0 {
        let mut tmp = [0u16; NUM_SPEEDS_TO_TRY];
        tmp.clone_from_slice(cdfs.split_at(NUM_SPEEDS_TO_TRY * (nibble - 1)).1.split_at(NUM_SPEEDS_TO_TRY).0);
        for i in 0..NUM_SPEEDS_TO_TRY {
            stride_pdf[i] -= tmp[i];
        }
        cm_pdf -= mixing_cdf[nibble - 1]
    }
    let mut stride_max = [0u16; NUM_SPEEDS_TO_TRY];
    stride_max.clone_from_slice(cdfs.split_at(NUM_SPEEDS_TO_TRY * 15).1);
    let cm_max = mixing_cdf[15];
    for i in 0..NUM_SPEEDS_TO_TRY {
        if stride_pdf[i] == 0 { 
            assert!(stride_pdf[i] != 0);
        }
        if stride_max[i] == 0 {
            assert!(stride_max[i] != 0);
        }
        let w;
        w = (1<<(BLEND_FIXED_POINT_PRECISION - 2)) ; // a quarter of weight to stride
        let combined_pdf = w * u32::from(stride_pdf[i]) + ((1<<BLEND_FIXED_POINT_PRECISION) - w) * u32::from(cm_pdf);
        let combined_max = w * u32::from(stride_max[i]) + ((1<<BLEND_FIXED_POINT_PRECISION) - w) * u32::from(cm_max);
        let del = FastLog2u16((combined_pdf >> BLEND_FIXED_POINT_PRECISION) as u16) - FastLog2u16((combined_max >> BLEND_FIXED_POINT_PRECISION) as u16);
        singleton_cost[i] -= del;
    }
}
fn compute_cost(singleton_cost: &mut [floatX;NUM_SPEEDS_TO_TRY],
                cdfs: &[u16],
                nibble_u8: u8) {
    assert_eq!(cdfs.len(), 16 * NUM_SPEEDS_TO_TRY);
    let nibble = nibble_u8 as usize & 0xf;
    let mut pdf = [0u16; NUM_SPEEDS_TO_TRY];
    pdf.clone_from_slice(cdfs.split_at(NUM_SPEEDS_TO_TRY * nibble).1.split_at(NUM_SPEEDS_TO_TRY).0);
    if nibble_u8 != 0 {
        let mut tmp = [0u16; NUM_SPEEDS_TO_TRY];
        tmp.clone_from_slice(cdfs.split_at(NUM_SPEEDS_TO_TRY * (nibble - 1)).1.split_at(NUM_SPEEDS_TO_TRY).0);
        for i in 0..NUM_SPEEDS_TO_TRY {
            pdf[i] -= tmp[i];
        }
    }
    let mut max = [0u16; NUM_SPEEDS_TO_TRY];
    max.clone_from_slice(cdfs.split_at(NUM_SPEEDS_TO_TRY * 15).1);
    for i in 0..NUM_SPEEDS_TO_TRY {
        if pdf[i] == 0 { 
            assert!(pdf[i] != 0);
        }
        if max[i] == 0 {
            assert!(max[i] != 0);
        }
        let del = FastLog2u16(pdf[i]) - FastLog2u16(max[i]);
        singleton_cost[i] -= del;
    }
}
fn update_one_cdf(cdfs: &mut [u16], nibble_u8: u8, speed_index: usize, cdf_stride: usize) {
    assert_eq!(cdfs.len(), 16 * cdf_stride);
    let mut overall_index = nibble_u8 as usize * cdf_stride;
    for _nibble in (nibble_u8 as usize & 0xf) .. 16 {
        cdfs[overall_index + speed_index] += SPEEDS_TO_SEARCH[speed_index];
        overall_index += cdf_stride;
    }
    let max_index = speed_index;
    if cdfs[15 * cdf_stride + max_index] >= MAXES_TO_SEARCH[max_index] {
        const CDF_BIAS:[u16;16] = [1,2,3,4,5,6,7,8,9,10,11,12,13,14,15,16];
        for nibble_index in 0..16  {
            let tmp = &mut cdfs[nibble_index * cdf_stride + max_index];
            *tmp = (tmp.wrapping_add(CDF_BIAS[nibble_index])).wrapping_sub(tmp.wrapping_add(CDF_BIAS[nibble_index]) >> 2);
        }
    }
}

fn update_cdf(cdfs: &mut [u16],
              nibble_u8: u8) {
    assert_eq!(cdfs.len(), 16 * NUM_SPEEDS_TO_TRY);
    let mut overall_index = nibble_u8 as usize * NUM_SPEEDS_TO_TRY;
    for _nibble in (nibble_u8 as usize & 0xf) .. 16 {
        for speed_index in 0..NUM_SPEEDS_TO_TRY {
            cdfs[overall_index + speed_index] += SPEEDS_TO_SEARCH[speed_index];
        }
        overall_index += NUM_SPEEDS_TO_TRY;
    }
    overall_index = 0;
    for nibble in 0 .. 16 {
        for speed_index in 0..NUM_SPEEDS_TO_TRY {
            if nibble == 0 {
                assert!(cdfs[overall_index + speed_index] != 0);
            } else {
                assert!(cdfs[overall_index + speed_index]  - cdfs[overall_index + speed_index - NUM_SPEEDS_TO_TRY]  != 0);
            }
        }
        overall_index += NUM_SPEEDS_TO_TRY;
    }
    for max_index in 0..NUM_SPEEDS_TO_TRY {
        if cdfs[15 * NUM_SPEEDS_TO_TRY + max_index] >= MAXES_TO_SEARCH[max_index] {
            const CDF_BIAS:[u16;16] = [1,2,3,4,5,6,7,8,9,10,11,12,13,14,15,16];
            for nibble_index in 0..16  {
                let tmp = &mut cdfs[nibble_index * NUM_SPEEDS_TO_TRY + max_index];
                *tmp = (tmp.wrapping_add(CDF_BIAS[nibble_index])).wrapping_sub(tmp.wrapping_add(CDF_BIAS[nibble_index]) >> 2);
            }
        }
    }
    overall_index = 0;
    for nibble in 0 .. 16 {
        for speed_index in 0..NUM_SPEEDS_TO_TRY {
            if nibble == 0 {
                assert!(cdfs[overall_index + speed_index] != 0);
            } else {
                assert!(cdfs[overall_index + speed_index]  - cdfs[overall_index + speed_index - NUM_SPEEDS_TO_TRY]  != 0);
            }
        }
        overall_index += NUM_SPEEDS_TO_TRY;
    }
}

fn extract_single_cdf(cdf_bundle:&[u16], index:usize) -> [u16;16] {
    assert_eq!(cdf_bundle.len(), 16 * NUM_SPEEDS_TO_TRY);
    assert!(index < NUM_SPEEDS_TO_TRY);
    [
        cdf_bundle[index + 0 * NUM_SPEEDS_TO_TRY],
        cdf_bundle[index + 1 * NUM_SPEEDS_TO_TRY],
        cdf_bundle[index + 2 * NUM_SPEEDS_TO_TRY],
        cdf_bundle[index + 3 * NUM_SPEEDS_TO_TRY],
        cdf_bundle[index + 4 * NUM_SPEEDS_TO_TRY],
        cdf_bundle[index + 5 * NUM_SPEEDS_TO_TRY],
        cdf_bundle[index + 6 * NUM_SPEEDS_TO_TRY],
        cdf_bundle[index + 7 * NUM_SPEEDS_TO_TRY],
        cdf_bundle[index + 8 * NUM_SPEEDS_TO_TRY],
        cdf_bundle[index + 9 * NUM_SPEEDS_TO_TRY],
        cdf_bundle[index + 10 * NUM_SPEEDS_TO_TRY],
        cdf_bundle[index + 11 * NUM_SPEEDS_TO_TRY],
        cdf_bundle[index + 12 * NUM_SPEEDS_TO_TRY],
        cdf_bundle[index + 13 * NUM_SPEEDS_TO_TRY],
        cdf_bundle[index + 14 * NUM_SPEEDS_TO_TRY],
        cdf_bundle[index + 15 * NUM_SPEEDS_TO_TRY],
        ]
}

fn min_cost_index_for_speed(cost: &[floatX]) -> usize {
    assert_eq!(cost.len(), NUM_SPEEDS_TO_TRY);
    let mut min_cost = cost[0];
    let mut best_choice = 0;
    for i in 1..NUM_SPEEDS_TO_TRY {
        if cost[i] < min_cost {
            best_choice = i;
            min_cost = cost[i];
        }
    }
    best_choice
}
fn min_cost_speed_max(cost: &[floatX]) -> SpeedAndMax {
    let best_choice = min_cost_index_for_speed(cost);
    SpeedAndMax(
        SPEEDS_TO_SEARCH[best_choice],
        MAXES_TO_SEARCH[best_choice])
}

fn min_cost_value(cost: &[floatX]) -> floatX {
    let best_choice = min_cost_index_for_speed(cost);
    cost[best_choice]
}
fn cost_type_index(cm: bool, combined: bool) -> usize {
    if combined {
        2usize
    } else if cm {
        0usize
    } else {
        1usize
    }    
}


const SINGLETON_COMBINED_STRATEGY: usize = 2;
const SINGLETON_STRIDE_STRATEGY: usize = 1;
const SINGLETON_CM_STRATEGY: usize = 0;
    
pub struct ContextMapEntropy<'a,
                             AllocU16:alloc::Allocator<u16>,
                             AllocU32:alloc::Allocator<u32>,
                             AllocF:alloc::Allocator<floatX>,
                             > {
    input: InputPair<'a>,
    context_map: interface::PredictionModeContextMap<InputReferenceMut<'a>>,
    block_type: u8,
    local_byte_offset: usize,
    weight: [[Weights; NUM_SPEEDS_TO_TRY];2],
    _nop: AllocU32::AllocatedMemory,
    
    cm_priors: AllocU16::AllocatedMemory,
    stride_priors: AllocU16::AllocatedMemory,
    stride_pyramid_leaves: [u8; find_stride::NUM_LEAF_NODES],
    singleton_costs: [[[floatX;NUM_SPEEDS_TO_TRY];2];3],
    phantom: core::marker::PhantomData<AllocF>,
    best_cm_speed_index_low: u8,
    best_cm_speed_index: u8,
    phase1: bool,
}
impl<'a,
     AllocU16:alloc::Allocator<u16>,
     AllocU32:alloc::Allocator<u32>,
     AllocF:alloc::Allocator<floatX>,
     > ContextMapEntropy<'a, AllocU16, AllocU32, AllocF> {
   pub fn new(m16: &mut AllocU16,
              _m32: &mut AllocU32,
              _mf: &mut AllocF,
              input: InputPair<'a>,
              stride: [u8; find_stride::NUM_LEAF_NODES],
              prediction_mode: interface::PredictionModeContextMap<InputReferenceMut<'a>>,
              cdf_detection_quality: u8) -> Self {
      let cdf_detect = cdf_detection_quality != 0;
      let mut ret = ContextMapEntropy::<AllocU16, AllocU32, AllocF>{
         phantom:core::marker::PhantomData::<AllocF>::default(),
         input: input,
         context_map: prediction_mode,
         block_type: 0,
         local_byte_offset: 0,
         _nop:  AllocU32::AllocatedMemory::default(),
         cm_priors: if cdf_detect {m16.alloc_cell(CONTEXT_MAP_PRIOR_SIZE)} else {AllocU16::AllocatedMemory::default()},
         stride_priors: if cdf_detect {m16.alloc_cell(STRIDE_PRIOR_SIZE)} else {AllocU16::AllocatedMemory::default()},
         stride_pyramid_leaves: stride,
         weight:[[Weights::new(); NUM_SPEEDS_TO_TRY],
                 [Weights::new(); NUM_SPEEDS_TO_TRY]],
         singleton_costs:[[[0.0 as floatX;NUM_SPEEDS_TO_TRY];2];3],
         best_cm_speed_index_low: 0,
         best_cm_speed_index: 0,
         phase1: false,
      };
      if cdf_detect {
        init_cdfs(ret.cm_priors.slice_mut());
        init_cdfs(ret.stride_priors.slice_mut());
      }
      ret
   }
    pub fn finish_phase0(&mut self) {
       assert_eq!(self.phase1, false);
       let mut indices = [0usize; 2];
       for high in 0..2 {
           indices[high] = min_cost_index_for_speed(&self.singleton_costs[cost_type_index(true, false)][high][..]);
       }
       self.best_cm_speed_index_low = indices[0] as u8;
       self.best_cm_speed_index = indices[1] as u8;
       self.local_byte_offset = 0;
       self.phase1 = true;
   }
   pub fn take_prediction_mode(&mut self) -> interface::PredictionModeContextMap<InputReferenceMut<'a>> {
       core::mem::replace(&mut self.context_map, interface::PredictionModeContextMap::<InputReferenceMut<'a>>{
          literal_context_map:InputReferenceMut(&mut[]),
          predmode_speed_and_distance_context_map:InputReferenceMut(&mut[]),
       })
   }
   pub fn prediction_mode_mut(&mut self) -> &mut interface::PredictionModeContextMap<InputReferenceMut<'a>> {
       &mut self.context_map
   }
   #[inline]
   pub fn track_cdf_speed(&mut self,
                      _data: &[u8],
                      mut _prev_byte: u8, mut _prev_prev_byte: u8,
                          _block_type: u8) {
   }
   pub fn best_singleton_speeds(&self,
                                cm: bool,
                                combined: bool) -> ([SpeedAndMax;2], [floatX; 2]) {
       let cost_type_index = cost_type_index(cm, combined);
       let mut ret_cost = [self.singleton_costs[cost_type_index][0][0],
                           self.singleton_costs[cost_type_index][1][0]];
       let mut best_indexes = [0,0];
       for speed_index in 1..NUM_SPEEDS_TO_TRY {
           for highness in 0..2 {
               let cur_cost = self.singleton_costs[cost_type_index][highness][speed_index];
               if cur_cost < ret_cost[highness] {
                   best_indexes[highness] = speed_index;
                   ret_cost[highness] = cur_cost;
               }
           }
       }
       let ret_speed = [SpeedAndMax(SPEEDS_TO_SEARCH[best_indexes[0]],
                                    MAXES_TO_SEARCH[best_indexes[0]]),
                        SpeedAndMax(SPEEDS_TO_SEARCH[best_indexes[1]],
                                    MAXES_TO_SEARCH[best_indexes[1]])];
       (ret_speed, ret_cost)
   }
   pub fn best_speeds(&mut self, // mut due to helpers
                       cm:bool,
                       combined: bool) -> [SpeedAndMax;2] { 
       let mut ret = [SpeedAndMax(SPEEDS_TO_SEARCH[0],MAXES_TO_SEARCH[0]); 2];
       let cost_type_index = cost_type_index(cm, combined);
       for high in 0..2 {
           /*eprintln!("TRIAL {} {}", cm, combined);
           for i in 0..NUM_SPEEDS_TO_TRY {
               eprintln!("{},{} costs {:?}", SPEEDS_TO_SEARCH[i], MAXES_TO_SEARCH[i], self.singleton_costs[cost_type_index][high][i]);
           }*/
         ret[high] = min_cost_speed_max(&self.singleton_costs[cost_type_index][high][..]);
       }
       ret
   }
   pub fn best_speeds_costs(&mut self, // mut due to helpers
                            cm:bool,
                            combined: bool) -> [floatX;2] { 
       let cost_type_index = cost_type_index(cm, combined);
       let mut ret = [0.0 as floatX; 2];
       for high in 0..2 {
         ret[high] = min_cost_value(&self.singleton_costs[cost_type_index][high][..]);
       }
       ret
   }
   pub fn free(&mut self, m16: &mut AllocU16, _m32: &mut AllocU32, _mf64: &mut AllocF) {
        m16.free_cell(core::mem::replace(&mut self.cm_priors, AllocU16::AllocatedMemory::default()));
        m16.free_cell(core::mem::replace(&mut self.stride_priors, AllocU16::AllocatedMemory::default()));
   }
   fn update_cost_phase0(&mut self, stride_prior: u8, cm_prior: usize, precursor_prior: usize, literal: u8) {
       let upper_nibble = (literal >> 4);
       let lower_nibble = literal & 0xf;
       {
           let cm_cdf_high = get_cm_cdf_high(self.cm_priors.slice_mut(), cm_prior);
           compute_cost(&mut self.singleton_costs[SINGLETON_CM_STRATEGY][1],
                        cm_cdf_high, upper_nibble);
           update_cdf(cm_cdf_high, upper_nibble);
       }
       {
           let cm_cdf_low = get_cm_cdf_low(self.cm_priors.slice_mut(), cm_prior, upper_nibble);
           compute_cost(&mut self.singleton_costs[SINGLETON_CM_STRATEGY][0],
                        cm_cdf_low, lower_nibble);
           update_cdf(cm_cdf_low, lower_nibble);
       }
       {
           let stride_cdf_high = get_stride_cdf_high(self.stride_priors.slice_mut(), stride_prior, precursor_prior);
           compute_cost(&mut self.singleton_costs[SINGLETON_STRIDE_STRATEGY][1],
                        stride_cdf_high, upper_nibble);
           update_cdf(stride_cdf_high, upper_nibble);
       }
       {
           let stride_cdf_low = get_stride_cdf_low(self.stride_priors.slice_mut(), stride_prior, precursor_prior, upper_nibble);
           compute_cost(&mut self.singleton_costs[SINGLETON_STRIDE_STRATEGY][0],
                        stride_cdf_low,
                        lower_nibble);
           update_cdf(stride_cdf_low, lower_nibble);
       }
   }
   fn update_cost_phase1(&mut self, stride_prior: u8, cm_prior: usize, literal: u8, cm_speed_index_low: usize, cm_speed_index: usize) {
       let upper_nibble = (literal >> 4);
       let lower_nibble = literal & 0xf;
       let provisional_cm_high_cdf: [u16; 16];
       let provisional_cm_low_cdf: [u16; 16];
       {
           let cm_cdf_high = get_cm_cdf_high(self.cm_priors.slice_mut(), cm_prior);
           provisional_cm_high_cdf = extract_single_cdf(cm_cdf_high, cm_speed_index);
       }
       {
           let cm_cdf_low = get_cm_cdf_low(self.cm_priors.slice_mut(), cm_prior, upper_nibble);
           provisional_cm_low_cdf = extract_single_cdf(cm_cdf_low, cm_speed_index_low);
       }
       {
           let stride_cdf_high = get_stride_cdf_high(self.stride_priors.slice_mut(), stride_prior, cm_prior);
           compute_combined_cost(&mut self.singleton_costs[SINGLETON_COMBINED_STRATEGY][1],
                                 stride_cdf_high, provisional_cm_high_cdf, upper_nibble, &mut self.weight[1]);
           update_cdf(stride_cdf_high, upper_nibble);
       }
       {
           let stride_cdf_low = get_stride_cdf_low(self.stride_priors.slice_mut(), stride_prior, cm_prior, upper_nibble);
           compute_combined_cost(&mut self.singleton_costs[SINGLETON_COMBINED_STRATEGY][0],
                                 stride_cdf_low, provisional_cm_low_cdf, lower_nibble, &mut self.weight[0]);
           update_cdf(stride_cdf_low, lower_nibble);
       }
       {
           let cm_cdf_high = get_cm_cdf_high(self.cm_priors.slice_mut(), cm_prior);
           update_one_cdf(cm_cdf_high, upper_nibble, cm_speed_index, NUM_SPEEDS_TO_TRY);
       }
       {
           let cm_cdf_low = get_cm_cdf_low(self.cm_priors.slice_mut(), cm_prior, upper_nibble);
           update_one_cdf(cm_cdf_low, lower_nibble, cm_speed_index_low, NUM_SPEEDS_TO_TRY);
       }
   }
}

fn Context(p1: u8, p2: u8, mode: ContextType) -> u8 {
  match mode {
    ContextType::CONTEXT_LSB6 => {
      return (p1 as (i32) & 0x3fi32) as (u8);
    }
    ContextType::CONTEXT_MSB6 => {
      return (p1 as (i32) >> 2i32) as (u8);
    }
    ContextType::CONTEXT_UTF8 => {
      return (kUTF8ContextLookup[p1 as (usize)] as (i32) |
              kUTF8ContextLookup[(p2 as (i32) + 256i32) as (usize)] as (i32)) as (u8);
    }
    ContextType::CONTEXT_SIGNED => {
      return ((kSigned3BitContextLookup[p1 as (usize)] as (i32) << 3i32) +
              kSigned3BitContextLookup[p2 as (usize)] as (i32)) as (u8);
    }
  }
  //  0i32 as (u8)
}

fn compute_huffman_table_index_for_context_map<SliceType: alloc::SliceWrapper<u8> > (
    prev_byte: u8,
    prev_prev_byte: u8,
    context_map: &interface::PredictionModeContextMap<SliceType>,
    block_type: u8,
) -> (usize, u8) {
    let prior = Context(prev_byte, prev_prev_byte, context_map.literal_prediction_mode().to_context_enum().unwrap());
    assert!(prior < 64);
    let context_map_index = ((block_type as usize)<< 6) | prior as usize;
    if context_map_index < context_map.literal_context_map.slice().len() {
        (context_map.literal_context_map.slice()[context_map_index] as usize, prior)
    } else {
        (prior as usize, prior)
    }
}



impl<'a, 'b, AllocU16: alloc::Allocator<u16>,
     AllocU32:alloc::Allocator<u32>,
     AllocF: alloc::Allocator<floatX>> interface::CommandProcessor<'b> for ContextMapEntropy<'a, AllocU16, AllocU32, AllocF> {
    fn push<Cb: FnMut(&[interface::Command<InputReference>])>(&mut self,
                                                              val: interface::Command<InputReference<'b>>,
                                                              callback: &mut Cb) {
        match val {
           interface::Command::BlockSwitchCommand(_) |
           interface::Command::BlockSwitchDistance(_) |
           interface::Command::PredictionMode(_) => {}
           interface::Command::Copy(ref copy) => {
             self.local_byte_offset += copy.num_bytes as usize;
           },
           interface::Command::Dict(ref dict) => {
             self.local_byte_offset += dict.final_size as usize;
           },
           interface::Command::BlockSwitchLiteral(block_type) => self.block_type = block_type.block_type(),
           interface::Command::Literal(ref lit) => {
               let stride = self.stride_pyramid_leaves[self.local_byte_offset * 8 / self.input.len()] as usize;
               let mut priors= [0u8; 8];
               for poffset in 0..core::cmp::max((stride & 7) + 1, 2) {
                   if self.local_byte_offset > poffset {
                       priors[7 - poffset] = self.input[self.local_byte_offset - poffset -  1];
                   }
               }
               let mut cur = 0usize;
               for literal in lit.data.slice().iter() {
                   let (huffman_table_index, raw_prior) = compute_huffman_table_index_for_context_map(priors[(cur + 7)&7], priors[(cur + 6) &7], &self.context_map, self.block_type);
                   if self.phase1 {
                       let ilow = self.best_cm_speed_index_low as usize;
                       let ihigh = self.best_cm_speed_index as usize;
                       self.update_cost_phase1(priors[(cur + 7 - stride) & 7], huffman_table_index, *literal, ilow, ihigh);
                   } else {
                       self.update_cost_phase0(priors[(cur + 7 - stride) & 7], huffman_table_index, raw_prior as usize, *literal);
                   }
                   priors[cur & 7] = *literal;
                   cur += 1;
                   cur &= 7;
               }
               self.local_byte_offset += lit.data.slice().len();
           }
        }
        let cbval = [val];
        callback(&cbval[..]);
    }
}
