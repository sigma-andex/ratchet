use std::cmp::Ordering;

use derive_new::new;
use encase::ShaderType;

use crate::{
    gpu::{
        BindGroupLayoutDescriptor, BindGroupLayoutHandle, ComputePipelineDescriptor, CpuUniform,
        KernelElement, PipelineLayoutDescriptor, WgpuDevice, WorkgroupCount,
    },
    rvec, wgc, CompiledOp, DType, Enforcer, OpMetadata, Operation, OperationError, RVec, Shape,
    StorageView, Tensor,
};

// Defines a matrix multiplication operation.
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub(crate) struct MatmulSpec {
    a_dt: DType,
    b_dt: DType,
    a_shape: Shape,
    b_shape: Shape,
    c_shape: Shape,
    a_stack: usize,
    b_stack: usize,
    c_stack: usize,
    stack_shape: Shape, //N-D matmul is handled by stacking the first N-2 dimensions
}

impl MatmulSpec {
    pub fn new(A: &Tensor, B: &Tensor, C: &Tensor) -> Self {
        let mut a_shape = A.shape().clone();
        let mut b_shape = B.shape().clone();
        let mut c_shape = C.shape().clone();
        let a_dt = A.dt();
        let b_dt = B.dt();

        if (a_shape.rank() < 2) || (b_shape.rank() < 2) {
            panic!("MatMul: inputs must be at least 2D");
        }

        match a_shape.rank().cmp(&b_shape.rank()) {
            Ordering::Less => {
                a_shape.left_pad_to(1, b_shape.rank());
            }
            Ordering::Greater => {
                b_shape.left_pad_to(1, a_shape.rank());
            }
            _ => {}
        };

        let _b_rank = b_shape.rank();

        let stack_dims = c_shape.rank() - 2;
        let stack_shape = c_shape.slice(0..stack_dims);

        let a_stack = a_shape.drain(0..stack_dims).product();
        let b_stack = b_shape.drain(0..stack_dims).product();
        let c_stack = c_shape.drain(0..stack_dims).product();

        if a_stack != 1 && b_stack != 1 {
            //Here we want all of the stacks to be equal
            //OR A or B to be 1
            assert!(a_stack == b_stack && b_stack == c_stack);
        }

        if a_shape.rank() == 1 {
            a_shape.insert(0, 1);
        }

        if b_shape.rank() == 1 {
            b_shape.insert(0, 1);
        }

        log::debug!(
            "MatMul stacking: left {} right {} stack_dims={} stack_count={}",
            a_shape,
            b_shape,
            stack_dims,
            stack_shape.numel(),
        );
        Self {
            a_dt,
            b_dt,
            a_shape,
            b_shape,
            c_shape,
            a_stack,
            b_stack,
            c_stack,
            stack_shape,
        }
    }

    pub fn select_kernel_element(&self) -> KernelElement {
        log::debug!(
            "select_kernel: m={} n={} k={}",
            self.m(),
            self.n(),
            self.k()
        );

        let checks = [
            self.k(),
            self.n(),
            self.a_shape.numel(),
            self.b_shape.numel(),
            self.c_shape.numel(),
        ];

        if checks.iter().all(|&x| x % 4 == 0) {
            KernelElement::Vec4
        } else if checks.iter().all(|&x| x % 2 == 0) {
            KernelElement::Vec2
        } else {
            KernelElement::Scalar
        }
    }

    pub fn tile_sizes(&self) -> (Option<usize>, Option<usize>) {
        let sizes = [32, 16];

        let checker = |dims: [usize; 2]| {
            sizes
                .iter()
                .find(|&size| dims.iter().all(|&dim| dim % size == 0))
                .copied()
        };

        (checker([self.m(), self.k()]), checker([self.k(), self.n()]))
    }

    pub fn m(&self) -> usize {
        self.a_shape[0]
    }

    pub fn k(&self) -> usize {
        self.a_shape[1]
    }

    pub fn n(&self) -> usize {
        self.b_shape[1]
    }

    pub fn a_stack(&self) -> usize {
        self.a_stack
    }

    pub fn b_stack(&self) -> usize {
        self.b_stack
    }

    pub fn c_stack(&self) -> usize {
        self.c_stack
    }

    pub fn stacks(&self) -> usize {
        self.stack_shape.numel()
    }

    pub fn b_dt(&self) -> DType {
        self.b_dt
    }

    pub fn stacked_shapes(&self) -> (Shape, Shape, Shape) {
        let mut a_shape = self.a_shape.clone();
        let mut b_shape = self.b_shape.clone();
        let mut c_shape = self.c_shape.clone();
        a_shape.insert(0, self.stacks());
        b_shape.insert(0, self.stacks());
        c_shape.insert(0, self.stacks());
        (a_shape, b_shape, c_shape)
    }
}

#[derive(new, Debug, Clone)]
pub struct Matmul {
    lhs: Tensor,
    rhs: Tensor,
}

#[allow(clippy::too_many_arguments)]
#[derive(Debug, Clone, ShaderType)]
pub struct MatmulMeta {
    M: u32,
    N: u32,
    K: u32,
    MD2: u32,
    ND2: u32,
    KD2: u32,
    MD4: u32,
    ND4: u32,
    KD4: u32,
    A_OFFSET: u32, //batch offset
    B_OFFSET: u32,
    C_OFFSET: u32,
}

impl MatmulMeta {
    pub fn new(M: u32, N: u32, K: u32, A_OFFSET: u32, B_OFFSET: u32, C_OFFSET: u32) -> Self {
        Self {
            M,
            N,
            K,
            MD2: M / 2,
            ND2: N / 2,
            KD2: K / 2,
            MD4: M / 4,
            ND4: N / 4,
            KD4: K / 4,
            A_OFFSET,
            B_OFFSET,
            C_OFFSET,
        }
    }
}

impl OpMetadata for MatmulMeta {}

impl Operation for Matmul {
    type Meta = MatmulMeta;

    fn name(&self) -> &'static str {
        "Matmul"
    }

    fn srcs(&self) -> RVec<&Tensor> {
        rvec![&self.lhs, &self.rhs]
    }

    fn storage_layout(&self, device: &WgpuDevice) -> Result<BindGroupLayoutHandle, OperationError> {
        Ok(device.get_or_create_bind_group_layout(&BindGroupLayoutDescriptor::ternary())?)
    }

    fn compile(
        &self,
        dst: &Tensor,
        uniform: &mut CpuUniform,
        device: &WgpuDevice,
    ) -> Result<CompiledOp, OperationError> {
        let A = &self.lhs;
        let B = &self.rhs;
        let C = dst;
        let spec = MatmulSpec::new(A, B, C);

        let kernel_elem = spec.select_kernel_element();

        let M = spec.m() as u32;
        let N = spec.n() as u32;
        let K = spec.k() as u32;

        //If the stack is 1, we don't want to offset the data
        fn calculate_offset(stack: u32, dim1: u32, dim2: u32, kernel_elem: &KernelElement) -> u32 {
            if stack == 1 {
                0
            } else {
                (dim1 * dim2) / kernel_elem.as_size() as u32
            }
        }

        let a_offset = calculate_offset(spec.a_stack() as _, M, K, &kernel_elem);
        let b_offset = calculate_offset(spec.b_stack() as _, K, N, &kernel_elem);
        let c_offset = calculate_offset(spec.c_stack() as _, M, N, &kernel_elem);

        let metadata = MatmulMeta::new(M, N, K, a_offset, b_offset, c_offset);
        let offset = uniform.write(&metadata)?;

        let group_x = WorkgroupCount::div_ceil(spec.m(), 8) as _;
        let group_y = WorkgroupCount::div_ceil(spec.n(), 8 * kernel_elem.as_size()) as u32;

        let storage_layout = self.storage_layout(device)?;
        let uniform_layout =
            device.get_or_create_bind_group_layout(&BindGroupLayoutDescriptor::uniform())?;
        let pipeline_layout = device.get_or_create_pipeline_layout(&PipelineLayoutDescriptor {
            entries: rvec![storage_layout, uniform_layout],
        })?;

        let pipeline_handle =
            device.get_or_create_compute_pipeline(&ComputePipelineDescriptor {
                pipeline_layout,
                kernel_key: "qgemm",
                elem: kernel_elem,
            })?;

        let storage_bind_groups = CompiledOp::create_storage_bind_groups(
            &self.srcs(),
            dst,
            rvec![storage_layout],
            device,
        );

        Ok(CompiledOp::new(
            pipeline_handle,
            wgc![group_x, group_y, spec.stacks() as _],
            storage_bind_groups,
            offset as _,
        ))
    }

    fn infer_output(&self, srcs: &[&Tensor]) -> Result<StorageView, OperationError> {
        //TODO: THIS IS WRONG
        Ok(srcs[0].view().clone())
    }

    fn check_invariants(srcs: &[&Tensor]) -> Result<(), OperationError> {
        Enforcer::check_input_arity(srcs, 2)?;
        Enforcer::check_dtype_match(srcs)?;
        Ok(())
    }
}
